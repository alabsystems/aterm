// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Token-bucket rate limiter for policy [`RateLimit`] entries.

use std::collections::HashMap;
use std::time::Duration;

use aterm_time::Instant;

use crate::{Policy, RateLimit};

/// The clock a debit reads, which the caller supplies: the crate offers no system clock, so a terminal engine debits at the instant its parse was given.
pub trait TimeSource {
    /// Return the current instant.
    fn now(&self) -> Instant;
}

/// Point-in-time balance and refill anchor of one [`TokenBucket`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BucketSnapshot {
    /// Current token balance (`0.0..=capacity_bytes as f64`).
    pub tokens: f64,
    /// Last refill instant, or `None` before the bucket's first debit.
    pub last_refill: Option<Instant>,
}

/// Token-bucket state for one named [`RateLimit`] entry.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    /// Current token balance. Saturates at `capacity_bytes` after refill.
    tokens: f64,
    /// Maximum token count (burst capacity).
    capacity_bytes: u64,
    /// Token refill rate in tokens-per-second.
    refill_per_second: u64,
    /// Hard cap on a single `try_consume(amount)` call, independent of
    /// the bucket level.
    per_sequence_max: u64,
    /// Last refill instant. `None` until the first `try_consume` seeds it,
    /// so construction needs no clock.
    last_refill: Option<Instant>,
}

impl TokenBucket {
    /// Construct a bucket from a [`RateLimit`] configuration.
    #[must_use]
    #[inline]
    pub const fn from_config(cfg: &RateLimit) -> Self {
        let capacity_bytes = cfg.capacity_bytes as u64;
        Self {
            #[allow(
                clippy::cast_precision_loss,
                reason = "capacity is bytes-scale; f64 has >40 bits of precision for any realistic cap"
            )]
            tokens: capacity_bytes as f64,
            capacity_bytes,
            refill_per_second: cfg.refill_per_second as u64,
            per_sequence_max: cfg.per_sequence_max as u64,
            last_refill: None,
        }
    }

    /// Attempt to deduct `amount` tokens using the given time source.
    pub fn try_consume<T: TimeSource>(&mut self, amount: u64, clock: &T) -> bool {
        if self.capacity_bytes == 0 {
            return false;
        }
        if self.per_sequence_max > 0 && amount > self.per_sequence_max {
            return false;
        }
        if amount > self.capacity_bytes {
            return false;
        }

        let now = clock.now();
        self.refill(now);

        #[allow(
            clippy::cast_precision_loss,
            reason = "amount bounded by capacity_bytes (u64); f64 precision sufficient"
        )]
        let needed = amount as f64;
        if self.tokens >= needed {
            self.tokens -= needed;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time since the last refill.
    fn refill(&mut self, now: Instant) {
        let Some(last) = self.last_refill else {
            self.last_refill = Some(now);
            return;
        };

        let elapsed = now.saturating_duration_since(last);
        if elapsed == Duration::ZERO {
            return;
        }

        #[allow(
            clippy::cast_precision_loss,
            reason = "refill rate is tokens/sec; f64 has >40 bits of precision for any realistic rate"
        )]
        let rate = self.refill_per_second as f64;
        let earned = rate * elapsed.as_secs_f64();
        #[allow(
            clippy::cast_precision_loss,
            reason = "capacity is bytes-scale; f64 precision sufficient for any realistic cap"
        )]
        let cap = self.capacity_bytes as f64;
        self.tokens = (self.tokens + earned).min(cap);
        self.last_refill = Some(now);
    }

    /// Capture this bucket's live balance and refill anchor.
    #[must_use]
    #[inline]
    pub const fn snapshot(&self) -> BucketSnapshot {
        BucketSnapshot {
            tokens: self.tokens,
            last_refill: self.last_refill,
        }
    }

    /// Restore a previously captured balance and refill anchor, clamping `tokens` into `0.0..=capacity_bytes`.
    #[inline]
    pub fn restore_snapshot(&mut self, snap: BucketSnapshot) {
        #[allow(
            clippy::cast_precision_loss,
            reason = "capacity is bytes-scale; f64 precision sufficient for any realistic cap"
        )]
        let cap = self.capacity_bytes as f64;
        self.tokens = if snap.tokens.is_finite() {
            snap.tokens.clamp(0.0, cap)
        } else {
            0.0
        };
        if self.tokens == 0.0 {
            self.tokens = 0.0;
        }
        self.last_refill = snap.last_refill;
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes
    }
}

/// A rate-limit id pre-resolved to its bucket index in a [`RateLimiterSet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimitSlot(Option<usize>);

impl RateLimitSlot {
    /// The id is not declared by the active policy ⇒ unlimited.
    pub const UNDECLARED: Self = Self(None);

    /// `true` when the id this slot came from is declared by the policy.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn is_declared(self) -> bool {
        self.0.is_some()
    }
}

/// Collection of named [`TokenBucket`]s, built from a [`Policy`]'s `rate_limits` table.
#[derive(Debug, Clone, Default)]
pub struct RateLimiterSet {
    /// One bucket per DISTINCT declared id, in first-declaration order.
    buckets: Vec<TokenBucket>,
    /// Declared id → its index in `buckets`.
    slots: HashMap<String, usize>,
}

impl RateLimiterSet {
    /// The heap bytes the set holds past its own size: its buckets and its id table.
    #[must_use]
    pub(crate) fn heap_bytes(&self) -> usize {
        let Self { buckets, slots } = self;
        slots.keys().fold(
            buckets
                .capacity()
                .saturating_mul(std::mem::size_of::<TokenBucket>())
                .saturating_add(aterm_alloc::heap::table_bytes::<(String, usize)>(
                    slots.capacity(),
                )),
            |sum, id| sum.saturating_add(id.capacity()),
        )
    }

    /// Build the set from a [`Policy`]'s `rate_limits` table.
    #[must_use]
    pub fn from_policy(policy: &Policy) -> Self {
        let cap = policy.rate_limits.len().min(1024);
        let mut buckets = Vec::with_capacity(cap);
        let mut slots = HashMap::with_capacity(cap);
        for cfg in &policy.rate_limits {
            let bucket = TokenBucket::from_config(cfg);
            match slots.get(&cfg.id) {
                Some(&idx) => {
                    if let Some(existing) = buckets.get_mut(idx) {
                        *existing = bucket;
                    }
                }
                None => {
                    slots.insert(cfg.id.clone(), buckets.len());
                    buckets.push(bucket);
                }
            }
        }
        Self { buckets, slots }
    }

    /// Resolve a rate-limit id to its bucket slot, for call sites that debit the same id many times under one policy.
    #[must_use]
    #[inline]
    pub fn slot(&self, id: &str) -> RateLimitSlot {
        RateLimitSlot(self.slots.get(id).copied())
    }

    /// Attempt to debit `amount` tokens from the bucket named by `id`.
    #[inline]
    pub fn try_consume<T: TimeSource>(&mut self, id: &str, amount: u64, clock: &T) -> bool {
        let slot = self.slot(id);
        self.try_consume_slot(slot, amount, clock)
    }

    /// Attempt to debit `amount` tokens from the bucket a [`RateLimitSlot`] names — the string-hash-free form of [`Self::try_consume`].
    #[inline]
    pub fn try_consume_slot<T: TimeSource>(
        &mut self,
        slot: RateLimitSlot,
        amount: u64,
        clock: &T,
    ) -> bool {
        let Some(idx) = slot.0 else {
            return true;
        };
        match self.buckets.get_mut(idx) {
            Some(bucket) => bucket.try_consume(amount, clock),
            None => true,
        }
    }

    /// Snapshot every declared bucket in first-declaration order.
    #[must_use]
    pub fn snapshots(&self) -> Vec<BucketSnapshot> {
        self.buckets.iter().map(TokenBucket::snapshot).collect()
    }

    /// Restore bucket snapshots in first-declaration order.
    pub fn restore_snapshots(&mut self, snaps: &[BucketSnapshot]) {
        for (bucket, &snap) in self.buckets.iter_mut().zip(snaps.iter()) {
            bucket.restore_snapshot(snap);
        }
    }

    /// Return the earliest `last_refill` instant across every declared bucket.
    #[must_use]
    pub fn earliest_refill(&self) -> Option<Instant> {
        self.buckets.iter().filter_map(|b| b.last_refill).min()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn get(&self, id: &str) -> Option<&TokenBucket> {
        self.buckets.get(*self.slots.get(id)?)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;
