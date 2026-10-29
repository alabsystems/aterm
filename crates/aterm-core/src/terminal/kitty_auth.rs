// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Structural capability gate on the Kitty non-direct medium resolver (`t=f`, `t=t`, `t=s`).
//!
//! The resolver reads a host file, a temp file it then removes, or a shared-memory object it then
//! unlinks, by a name the program wrote. It lives in [`KittyResolverSlot`], whose field only this
//! module reads: a name reaches it only through [`KittyResolverSlot::resolve`], which takes the
//! capability [`KittyResolverSlot::try_mint`] returns for a non-direct medium while the host has
//! installed a resolver ([`super::Terminal::set_kitty_file_resolver`], and none since
//! [`super::Terminal::clear_kitty_file_resolver`]).

use std::sync::atomic::{AtomicU64, Ordering};

use super::kitty_graphics::KittyMedium;

/// The host's resolver for a Kitty non-direct medium: the bytes `name` holds, or `None`.
pub(super) type KittyFileResolver = Box<dyn Fn(KittyMedium, &str) -> Option<Vec<u8>> + Send>;

/// Proof that a non-direct transmission may reach the host's resolver, minted only by
/// [`KittyResolverSlot::try_mint`].
#[must_use = "KittyMediumCapability has no effect unless passed to resolve"]
pub(super) struct KittyMediumCapability {
    _seal: super::Seal,
}

/// The host's Kitty medium resolver, when one is installed.
pub(super) struct KittyResolverSlot {
    resolver: Option<KittyFileResolver>,
    loaded: AtomicU64,
}

impl KittyResolverSlot {
    #[inline]
    pub(super) const fn new() -> Self {
        Self {
            resolver: None,
            loaded: AtomicU64::new(0),
        }
    }

    /// Record that a non-direct medium payload built an image on the live engine.
    #[inline]
    pub(super) fn note_loaded(&self) {
        self.loaded.fetch_add(1, Ordering::Relaxed);
    }

    /// Monotonic count of non-direct medium payloads that built an image on this engine.
    #[inline]
    pub(super) fn loaded_count(&self) -> u64 {
        self.loaded.load(Ordering::Relaxed)
    }

    #[inline]
    pub(super) fn install(&mut self, resolver: KittyFileResolver) {
        self.resolver = Some(resolver);
    }

    #[inline]
    pub(super) fn uninstall(&mut self) {
        self.resolver = None;
    }

    /// Whether the host installed a resolver.
    #[inline]
    pub(super) const fn installed(&self) -> bool {
        self.resolver.is_some()
    }

    /// Mint a capability for `medium` when it is not direct and a resolver is installed.
    #[inline]
    pub(super) fn try_mint(&self, medium: KittyMedium) -> Option<KittyMediumCapability> {
        (medium != KittyMedium::Direct && self.installed())
            .then_some(KittyMediumCapability { _seal: super::Seal })
    }

    /// Ask the resolver for what `name` holds in `medium`, consuming the capability.
    #[inline]
    pub(super) fn resolve(
        &self,
        _token: KittyMediumCapability,
        medium: KittyMedium,
        name: &str,
    ) -> Option<Vec<u8>> {
        self.resolver
            .as_ref()
            .and_then(|resolve| resolve(medium, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NON_DIRECT: [KittyMedium; 3] = [
        KittyMedium::File,
        KittyMedium::TempFile,
        KittyMedium::SharedMemory,
    ];

    #[test]
    fn nothing_mints_without_a_resolver() {
        let slot = KittyResolverSlot::new();
        for medium in NON_DIRECT {
            assert!(slot.try_mint(medium).is_none(), "{medium:?}");
        }
    }

    #[test]
    fn an_uninstalled_resolver_mints_nothing() {
        let mut slot = KittyResolverSlot::new();
        slot.install(Box::new(|_, _| Some(vec![1])));
        slot.uninstall();
        assert!(!slot.installed());
        for medium in NON_DIRECT {
            assert!(slot.try_mint(medium).is_none(), "{medium:?}");
        }
    }

    #[test]
    fn a_direct_medium_never_mints() {
        let mut slot = KittyResolverSlot::new();
        slot.install(Box::new(|_, _| Some(vec![1])));
        assert!(slot.try_mint(KittyMedium::Direct).is_none());
    }

    #[test]
    fn a_minted_capability_reaches_the_resolver() {
        let mut slot = KittyResolverSlot::new();
        slot.install(Box::new(|medium, name| {
            Some(format!("{medium:?}:{name}").into_bytes())
        }));
        for medium in NON_DIRECT {
            let cap = slot.try_mint(medium).expect("minted");
            assert_eq!(
                slot.resolve(cap, medium, "n"),
                Some(format!("{medium:?}:n").into_bytes())
            );
        }
    }
}
