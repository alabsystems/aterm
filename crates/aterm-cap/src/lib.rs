// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owns-correctness capabilities.

use std::marker::PhantomData;

/// Trust tier carried by a capability — the effect layer's policy axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// From an unauthenticated / third-party source.
    Untrusted,
    /// From the trusted in-process core.
    Trusted,
    /// Backed by a machine-checked certificate (the only tier that may elide a runtime check; mirrors Trust's `Certified` assurance tier).
    Certified,
}

impl Tier {
    /// The variant's canonical name — identical to its `Debug` rendering. Used
    /// by the hand-written `Debug`/`Display` impls so they can format through
    /// [`std::fmt::Formatter::write_str`] (a single static-string write) rather
    /// than the `write!`/`format_args!` machinery.
    #[inline]
    #[must_use]
    const fn name(self) -> &'static str {
        match self {
            Tier::Untrusted => "Untrusted",
            Tier::Trusted => "Trusted",
            Tier::Certified => "Certified",
        }
    }

    /// Monotone numeric rank (`Untrusted = 0 < Trusted = 1 < Certified = 2`).
    #[inline]
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Tier::Untrusted => 0,
            Tier::Trusted => 1,
            Tier::Certified => 2,
        }
    }
}

/// A capability authorizing effect marker `E`, at a trust [`Tier`].
pub struct Cap<E> {
    tier: Tier,
    _effect: PhantomData<fn() -> E>,
    _seal: Seal,
}

/// A private, un-nameable witness.
#[derive(Clone, Copy)]
struct Seal;

impl<E> Cap<E> {
    /// The trust tier this capability was granted at.
    #[inline]
    #[must_use]
    pub const fn tier(&self) -> Tier {
        self.tier
    }

    /// Whether this capability meets a minimum required tier.
    #[inline]
    #[must_use]
    pub const fn satisfies(&self, min: Tier) -> bool {
        self.tier.rank() >= min.rank()
    }
}

impl<E> Clone for Cap<E> {
    #[inline]
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

/// The single minting authority.
pub struct Authority {
    _seal: Seal,
}

impl Authority {
    /// Mint the process root authority, from which any capability at any tier can be granted: the most powerful operation in the process.
    /// # Safety
    /// The caller must be the trusted launcher / process entry point, and must call this at most once, before any untrusted input is processed.
    /// Leaking the returned `Authority` (or caps minted from it) to untrusted code defeats the
    /// capability model.
    ///
    /// # Sealing
    /// Exists only under the `launcher-mint` cargo feature (or this crate's `test` cfg), which only
    /// the trusted launchers (`aterm-gui`, `aterm-cli`) enable; feature unification makes it
    /// nameable by every crate in such a build, so the `launcher-mint` census guard keeps every
    /// shipped call in those two crates.
    #[cfg(any(test, feature = "launcher-mint"))]
    #[doc(hidden)]
    #[inline]
    #[must_use]
    pub const unsafe fn root_authority() -> Authority {
        Authority { _seal: Seal }
    }

    /// Mint a capability for effect `E` at `tier`.
    #[inline]
    #[must_use]
    pub const fn grant<E>(&self, tier: Tier) -> Cap<E> {
        Cap {
            tier,
            _effect: PhantomData,
            _seal: Seal,
        }
    }
}

/// The effect-gate.
/// # Errors
/// Returns [`Denied`] when `cap.tier() < min`.
#[inline]
pub const fn require<E>(cap: &Cap<E>, min: Tier) -> Result<(), Denied> {
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

/// Zero-size effect markers naming a privileged effect a [`Cap`] authorizes.
pub mod effects {
    /// Authorizes spawning a child process (the PTY shell).
    pub enum Spawn {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use effects::Spawn;

    // Test helper: mint the root authority. SAFETY: a test process with no untrusted input
    // satisfies the trusted-launcher contract of `root_authority`.
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
        assert_eq!(
            require(&clip, Tier::Trusted),
            Err(Denied {
                have: Tier::Untrusted,
                need: Tier::Trusted
            })
        );
        assert_eq!(require(&clip, Tier::Untrusted), Ok(()));

        let certified: Cap<Spawn> = authority.grant(Tier::Certified);
        assert_eq!(require(&certified, Tier::Trusted), Ok(()));
    }
}
