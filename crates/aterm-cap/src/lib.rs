// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owns-correctness capabilities.
//!
//! A [`Cap<E>`] authorizes effect `E` at a trust [`Tier`]. A function that
//! performs a privileged effect takes a `&Cap<ThatEffect>`, so calling it
//! without the capability is a type error; [`require`] is the canonical gate.
//!
//! Two properties hold:
//!
//! 1. No struct-literal forgery: `Cap`'s and `Authority`'s fields are private,
//!    so capabilities flow only from a held [`Authority`] via
//!    [`Authority::grant`].
//! 2. No mint reachability (`docs/ATERM_DESIGN.md` §5.4): the one mint,
//!    [`Authority::root_authority`], is `unsafe` and exists only under the
//!    `launcher-mint` feature (or this crate's `test` cfg). Cargo unifies
//!    features, so the cfg seals it only in a graph with no launcher; in the
//!    shipped build the `launcher-mint` source guard (`aterm-census --guards`)
//!    holds it: outside aterm-gui and aterm-cli no shipped code names the mint
//!    and no shipped manifest enables the feature.

use std::marker::PhantomData;

/// Trust tier carried by a capability — the effect layer's policy axis.
///
/// Ordered: `Untrusted < Trusted < Certified`. A sink can require a minimum tier
/// (e.g. only `Certified` capabilities may elide a runtime safety check).
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// From an unauthenticated / third-party source.
    Untrusted,
    /// From the trusted in-process core.
    Trusted,
    /// Backed by a machine-checked certificate (the only tier that may elide a
    /// runtime check; mirrors Trust's `Certified` assurance tier).
    Certified,
}

impl Tier {
    /// The variant's canonical name — identical to its `Debug` rendering. Used
    /// by the hand-written `Debug`/`Display` impls so they can format through
    /// [`std::fmt::Formatter::write_str`] (a single static-string write) rather
    /// than the `write!`/`format_args!` machinery.
    #[must_use]
    const fn name(self) -> &'static str {
        match self {
            Tier::Untrusted => "Untrusted",
            Tier::Trusted => "Trusted",
            Tier::Certified => "Certified",
        }
    }
}

/// A capability authorizing effect marker `E`, at a trust [`Tier`].
///
/// The private `_seal` field prevents struct-literal forgery outside this
/// crate; the only source is [`Authority::grant`], which requires holding an
/// [`Authority`].
pub struct Cap<E> {
    tier: Tier,
    _effect: PhantomData<fn() -> E>,
    _seal: Seal,
}

/// A private, un-nameable witness. It is `Copy` (so a granted [`Cap`] can be
/// copied) but unconstructable outside this crate: copying one requires
/// already holding it, which requires the [`Authority`]. This blocks struct
/// literals only; the mint seal is the `launcher-mint` feature.
#[derive(Clone, Copy)]
struct Seal;

impl<E> Cap<E> {
    /// The trust tier this capability was granted at.
    #[must_use]
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Whether this capability meets a minimum required tier.
    #[must_use]
    pub fn satisfies(&self, min: Tier) -> bool {
        self.tier >= min
    }
}

// Capabilities are copyable witnesses. Hand-written (not derived) so the impls
// add no spurious `E: Clone`/`E: Copy` bound: `E` is a phantom effect marker.
impl<E> Clone for Cap<E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<E> Copy for Cap<E> {}

impl<E> std::fmt::Debug for Cap<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cap<")?;
        f.write_str(std::any::type_name::<E>())?;
        f.write_str(">(")?;
        f.write_str(self.tier.name())?;
        f.write_str(")")
    }
}

/// The single minting authority. Holding one is the right to grant capabilities;
/// it is created once at process start by the trusted launcher and handed only to
/// the trusted core. It is not `Clone`, so it cannot be duplicated into untrusted
/// hands.
pub struct Authority {
    _seal: Seal,
}

impl Authority {
    /// Mint the process root authority, from which any capability at any tier
    /// can be granted: the most powerful operation in the process.
    ///
    /// It is `unsafe` not for memory safety but so every mint site is a
    /// greppable, audited `unsafe` block; there is no safe public path to an
    /// `Authority`.
    ///
    /// # Safety
    ///
    /// The caller must be the trusted launcher / process entry point, and must
    /// call this at most once, before any untrusted input is processed. Leaking
    /// the returned `Authority` (or caps minted from it) to untrusted code
    /// defeats the capability model.
    ///
    /// # Sealing
    ///
    /// Exists only under the `launcher-mint` cargo feature (or this crate's
    /// `test` cfg), which only the trusted launchers (`aterm-gui`, `aterm-cli`)
    /// enable. Feature unification makes it nameable by every crate in a build
    /// that holds a launcher, so the `launcher-mint` source guard
    /// (`aterm-census --guards`) keeps every shipped call in those two crates.
    #[cfg(any(test, feature = "launcher-mint"))]
    #[doc(hidden)]
    #[must_use]
    pub unsafe fn root_authority() -> Authority {
        Authority { _seal: Seal }
    }

    /// Mint a capability for effect `E` at `tier`.
    #[must_use]
    pub fn grant<E>(&self, tier: Tier) -> Cap<E> {
        Cap {
            tier,
            _effect: PhantomData,
            _seal: Seal,
        }
    }
}

/// The effect-gate. A privileged operation calls `require(cap, min)`; it returns
/// `Ok(())` only if `cap` meets the minimum tier, so the operation cannot proceed
/// without an adequately-tiered capability (and, by the type, without a capability
/// *for that effect* at all). Returns the shortfall on denial.
///
/// # Errors
/// Returns [`Denied`] when `cap.tier() < min`.
pub fn require<E>(cap: &Cap<E>, min: Tier) -> Result<(), Denied> {
    if cap.satisfies(min) {
        Ok(())
    } else {
        Err(Denied {
            have: cap.tier(),
            need: min,
        })
    }
}

/// An effect was denied because the presented capability's tier was too low.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Denied {
    pub have: Tier,
    pub need: Tier,
}

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("capability denied: have ")?;
        f.write_str(self.have.name())?;
        f.write_str(", need at least ")?;
        f.write_str(self.need.name())
    }
}
impl std::error::Error for Denied {}

/// Zero-size effect markers naming a privileged effect a [`Cap`] authorizes. A
/// function performing the effect takes `&Cap<ThisEffect>`.
pub mod effects {
    /// Authorizes spawning a child process (the PTY shell). The spawn seam
    /// (`aterm-pty`) requires a `Cap<Spawn>` of at least `Trusted` tier.
    pub enum Spawn {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use effects::Spawn;

    // Test helper: mint the root authority. SAFETY: in a single-threaded test
    // process with no untrusted input, the trusted-launcher contract of
    // `root_authority` is trivially satisfied.
    fn test_authority() -> Authority {
        unsafe { Authority::root_authority() }
    }

    #[test]
    fn granted_cap_carries_its_tier() {
        let authority = test_authority();
        let spawn: Cap<Spawn> = authority.grant(Tier::Trusted);
        assert_eq!(spawn.tier(), Tier::Trusted);
        assert!(spawn.satisfies(Tier::Untrusted));
        assert!(spawn.satisfies(Tier::Trusted));
        assert!(!spawn.satisfies(Tier::Certified));
    }

    #[test]
    fn require_gates_on_tier() {
        let authority = test_authority();
        let clip: Cap<Spawn> = authority.grant(Tier::Untrusted);
        // An effect needing Trusted is denied to an Untrusted cap.
        assert_eq!(
            require(&clip, Tier::Trusted),
            Err(Denied {
                have: Tier::Untrusted,
                need: Tier::Trusted
            })
        );
        // An effect needing Untrusted is allowed.
        assert_eq!(require(&clip, Tier::Untrusted), Ok(()));

        let certified: Cap<Spawn> = authority.grant(Tier::Certified);
        assert_eq!(require(&certified, Tier::Trusted), Ok(()));
    }
}
