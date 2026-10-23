// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Token-bucket rate limiter for policy [`RateLimit`] entries.

use std::collections::HashMap;
use std::time::Duration;

use aterm_time::Instant;

use crate::{Policy, RateLimit};

/// Abstraction over [`Instant::now`] so tests can step the clock without sleeping.
pub trait TimeSource {
    /// Return the current instant.
    fn now(&self) -> Instant;
}

/// Production time source — delegates to [`Instant::now`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl TimeSource for SystemClock {
    #[inline]
    fn now(&self) -> Instant {
        Instant::now()
    }
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
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Deterministic clock — advances only when explicitly stepped.
    struct FakeClock {
        now: Cell<Instant>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Cell::new(Instant::now()),
            }
        }

        fn advance(&self, by: Duration) {
            self.now.set(self.now.get() + by);
        }
    }

    impl TimeSource for FakeClock {
        fn now(&self) -> Instant {
            self.now.get()
        }
    }

    fn rl(id: &str, capacity: u32, refill: u32, per_seq: u32) -> RateLimit {
        RateLimit {
            id: id.to_owned(),
            capacity_bytes: capacity,
            refill_per_second: refill,
            per_sequence_max: per_seq,
        }
    }

    #[test]
    fn burst_capacity_drains_then_denies() {
        let mut bucket = TokenBucket::from_config(&rl("x", 200, 100, 0));
        let clock = FakeClock::new();

        let mut allowed = 0;
        let mut denied = 0;
        for _ in 0..10 {
            if bucket.try_consume(40, &clock) {
                allowed += 1;
            } else {
                denied += 1;
            }
        }

        assert_eq!(allowed, 5);
        assert_eq!(denied, 5);
    }

    #[test]
    fn refill_replenishes_after_cooldown() {
        let mut bucket = TokenBucket::from_config(&rl("x", 100, 1_000, 0));
        let clock = FakeClock::new();

        assert!(bucket.try_consume(100, &clock));
        assert!(!bucket.try_consume(1, &clock));

        clock.advance(Duration::from_millis(50));
        assert!(bucket.try_consume(50, &clock));
        assert!(!bucket.try_consume(1, &clock));
    }

    #[test]
    fn idle_bucket_caps_at_configured_max() {
        let mut bucket = TokenBucket::from_config(&rl("x", 100, 10_000, 0));
        let clock = FakeClock::new();

        assert!(bucket.try_consume(100, &clock));
        clock.advance(Duration::from_secs(10));
        assert!(bucket.try_consume(100, &clock));
        assert!(!bucket.try_consume(1, &clock));
    }

    #[test]
    fn oversized_request_always_denied_without_draining() {
        let mut bucket = TokenBucket::from_config(&rl("x", 100, 1_000, 0));
        let clock = FakeClock::new();

        assert!(!bucket.try_consume(101, &clock));
        assert!(bucket.try_consume(100, &clock));
    }

    #[test]
    fn zero_capacity_is_hard_block() {
        let mut bucket = TokenBucket::from_config(&rl("x", 0, 10_000, 0));
        let clock = FakeClock::new();
        assert!(!bucket.try_consume(1, &clock));
        assert!(!bucket.try_consume(0, &clock));
    }

    #[test]
    fn per_sequence_cap_rejects_oversized_call() {
        let mut bucket = TokenBucket::from_config(&rl("palette", 1_000, 1_000, 16));
        let clock = FakeClock::new();

        assert!(bucket.try_consume(16, &clock));
        assert!(!bucket.try_consume(17, &clock));
        assert!(bucket.try_consume(16, &clock));
    }

    #[test]
    fn per_sequence_cap_zero_disables_check() {
        let mut bucket = TokenBucket::from_config(&rl("response", 65_536, 102_400, 0));
        let clock = FakeClock::new();
        assert!(bucket.try_consume(65_536, &clock));
        assert!(!bucket.try_consume(1, &clock));
    }

    #[test]
    fn known_id_enforces_bucket() {
        let policy = Policy {
            schema_version: crate::SCHEMA_VERSION,
            profile: crate::Profile::Standard,
            defaults: crate::Defaults {
                unmatched: crate::Response::Drop,
                shell_integration_require_nonce: true,
            },
            rules: vec![],
            rate_limits: vec![rl("response", 100, 1_000, 0)],
        };
        let mut set = RateLimiterSet::from_policy(&policy);
        let clock = FakeClock::new();

        assert!(set.try_consume("response", 100, &clock));
        assert!(!set.try_consume("response", 1, &clock));
        assert!(set.try_consume("other", 1, &clock));
    }

    #[test]
    fn from_policy_collapses_duplicate_ids_last_wins() {
        let policy = Policy {
            schema_version: crate::SCHEMA_VERSION,
            profile: crate::Profile::Standard,
            defaults: crate::Defaults {
                unmatched: crate::Response::Drop,
                shell_integration_require_nonce: true,
            },
            rules: vec![],
            rate_limits: vec![rl("x", 10, 10, 0), rl("x", 100, 100, 0)],
        };
        let set = RateLimiterSet::from_policy(&policy);
        let bucket = set.get("x").expect("bucket x should exist");
        assert_eq!(bucket.capacity_bytes(), 100);
        assert_eq!(set.len(), 1);
    }

    fn two_bucket_policy() -> Policy {
        Policy {
            schema_version: crate::SCHEMA_VERSION,
            profile: crate::Profile::Standard,
            defaults: crate::Defaults {
                unmatched: crate::Response::Drop,
                shell_integration_require_nonce: true,
            },
            rules: vec![],
            rate_limits: vec![rl("response", 100, 1_000, 0), rl("palette", 10, 10, 0)],
        }
    }

    #[test]
    fn slot_debits_the_bucket_its_id_names() {
        let mut set = RateLimiterSet::from_policy(&two_bucket_policy());
        let clock = FakeClock::new();
        let response = set.slot("response");
        let palette = set.slot("palette");
        assert!(response.is_declared());
        assert!(palette.is_declared());
        assert_ne!(response, palette);

        assert!(set.try_consume_slot(response, 100, &clock));
        assert!(!set.try_consume_slot(response, 1, &clock));
        assert!(set.try_consume_slot(palette, 10, &clock));
        assert!(!set.try_consume_slot(palette, 1, &clock));
    }

    #[test]
    fn undeclared_slot_allows_like_an_unknown_id() {
        let mut set = RateLimiterSet::from_policy(&two_bucket_policy());
        let clock = FakeClock::new();
        let missing = set.slot("no-such-bucket");
        assert!(!missing.is_declared());
        assert_eq!(missing, RateLimitSlot::UNDECLARED);
        assert!(set.try_consume_slot(missing, 99_999, &clock));
        assert!(set.try_consume("no-such-bucket", 99_999, &clock));
        assert_eq!(RateLimitSlot::default(), RateLimitSlot::UNDECLARED);
    }

    #[test]
    fn slot_from_a_wider_set_stays_total() {
        let wide = RateLimiterSet::from_policy(&two_bucket_policy());
        let stale = wide.slot("palette");
        let narrow_policy = Policy {
            rate_limits: vec![rl("response", 100, 1_000, 0)],
            ..two_bucket_policy()
        };
        let mut narrow = RateLimiterSet::from_policy(&narrow_policy);
        let clock = FakeClock::new();
        assert!(narrow.try_consume_slot(stale, 99_999, &clock));
        assert!(narrow.try_consume("response", 100, &clock));
    }
}
