// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Opaque conditional-settlement observations, allocated by a host incarnation.

use std::sync::atomic::{AtomicU64, Ordering};

/// A host's shared source of digest confirmations, independent of either clock.
///
/// Native and browser hosts seed this once from their platform CSPRNG, then share it
/// across centers and their clones. Every accepted report takes a token from the same
/// sequence; entries must not increment their own tokens into another entry's range.
/// Independent hosts have a nonzero probability of overlapping sequences; this is a
/// freshness nonce, not a durable cross-process uniqueness proof.
#[derive(Debug)]
pub struct ConfirmationSource {
    next: AtomicU64,
}

impl ConfirmationSource {
    /// Start with a host-provided random seed. Missing entropy, zero and `u64::MAX`
    /// disable conditional settlement; there is no clock or deterministic fallback.
    #[must_use]
    pub const fn new(seed: Option<u64>) -> Self {
        let first = match seed {
            None | Some(0) => u64::MAX,
            Some(value) => value,
        };
        Self {
            next: AtomicU64::new(first),
        }
    }

    /// Fill one seed from the host CSPRNG. A failed fill discards all supplied bytes.
    /// This callback is invoked once; the source never retries or samples a clock.
    #[must_use]
    pub fn from_entropy(fill: impl FnOnce(&mut [u8]) -> bool) -> Self {
        let mut bytes = [0; 8];
        let seed = if fill(&mut bytes) {
            Some(u64::from_le_bytes(bytes))
        } else {
            None
        };
        Self::new(seed)
    }

    pub(crate) fn take(&self) -> u64 {
        let mut next = self.next.load(Ordering::Relaxed);
        loop {
            let Some(after) = next.checked_add(1) else {
                return u64::MAX;
            };
            match self
                .next
                .compare_exchange_weak(next, after, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(token) => return token,
                Err(current) => next = current,
            }
        }
    }
}
