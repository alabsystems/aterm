// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Token-bucket rate limiter for policy [`RateLimit`] entries. The
//! [`PolicyEngine`][pe] owns one [`TokenBucket`] per [`RateLimit`] id
//! declared on the active [`Policy`].
//!
//! # Semantics
//!
//! A [`TokenBucket`] enforces three independent limits that map 1-to-1 onto
//! the fields of [`RateLimit`]:
//!
//! * `capacity_bytes` — burst budget. Tokens accumulate up to this ceiling
//!   while idle and drain by `amount` on every successful consume.
//! * `refill_per_second` — steady-state rate. Tokens replenish linearly
//!   between calls. A rate of 0 freezes the bucket at the current balance.
//! * `per_sequence_max` — hard cap on any single `try_consume(amount)` call,
//!   independent of the current token balance. A value of 0 disables the
//!   per-call cap (only steady-state + burst apply). The OSC 4 / OSC 21
//!   palette handlers use it as their per-query pair cap.
//!
//! A bucket with `capacity_bytes = 0` is a hard "deny all" — useful as a
//! kill switch from a hot-swapped policy.
//!
//! # Time source abstraction
//!
//! Production callers use [`SystemClock`] (a zero-size wrapper over
//! [`Instant::now`]). Tests implement [`TimeSource`] with a clock that
//! advances only when stepped, so they never sleep.
//!
//! # Totality
//!
//! Every entry point is total and panic-free:
//!
//! * `try_consume(amount)` returns `false` on zero-capacity, per-sequence
//!   cap violations, oversized requests, and insufficient balance.
//! * No allocation on the hot path. Returns a [`Copy`] [`bool`].
//! * Arithmetic uses `f64` for sub-millisecond refill accumulation and
//!   saturates on the `Instant` delta.
//!
//! [pe]: crate::engine::PolicyEngine

use std::collections::HashMap;
use std::time::Duration;
// Not std's: `std::time::Instant::now()` panics on wasm32.
use aterm_time::Instant;

use crate::{Policy, RateLimit};

/// Abstraction over [`Instant::now`] so tests can step the clock without
/// sleeping. Production callers use [`SystemClock`].
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
///
/// Built from a [`RateLimit`] configuration via [`Self::from_config`]. Starts
/// full (`tokens == capacity_bytes`).
#[derive(Debug, Clone)]
pub struct TokenBucket {
    /// Current token balance. Saturates at `capacity_bytes` after refill.
    tokens: f64,
    /// Maximum token count (burst capacity).
    capacity_bytes: u64,
    /// Token refill rate in tokens-per-second.
    refill_per_second: u64,
    /// Hard cap on a single `try_consume(amount)` call, independent of
    /// the bucket level. `0` means "no per-call cap".
    per_sequence_max: u64,
    /// Last refill instant. `None` until the first `try_consume` seeds it,
    /// so construction needs no clock.
    last_refill: Option<Instant>,
}

impl TokenBucket {
    /// Construct a bucket from a [`RateLimit`] configuration.
    ///
    /// The bucket starts at full capacity.
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
    ///
    /// Returns `true` if the call is permitted and the balance was
    /// debited; `false` if the call is denied and the balance is
    /// unchanged. Denial occurs when any of the following hold:
    ///
    /// 1. `capacity_bytes == 0` (kill switch).
    /// 2. `per_sequence_max > 0 && amount > per_sequence_max` (per-call
    ///    cap exceeded).
    /// 3. `amount > capacity_bytes` (oversized request can never fit
    ///    even with a full bucket).
    /// 4. The current balance after refill is less than `amount`.
    ///
    /// A denial in cases 1–3 does not debit the bucket — a single
    /// pathological caller cannot drain the tokens available to
    /// well-behaved peers.
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
            // First call: seed the clock but don't grant extra tokens —
            // the bucket already starts at full capacity.
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
///
/// Resolves via [`RateLimiterSet::slot`] and debits via
/// [`RateLimiterSet::try_consume_slot`] to avoid hashing the string id on every
/// dispatch. [`Self::UNDECLARED`] (and any out-of-range index from a different
/// set) allows, matching [`RateLimiterSet::try_consume`]; callers must
/// re-resolve slots when swapping policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimitSlot(Option<usize>);

impl RateLimitSlot {
    /// The id is not declared by the active policy ⇒ unlimited.
    ///
    /// Also the [`Default`], so a host that has not resolved a slot yet is
    /// unlimited rather than accidentally pointed at bucket 0.
    pub const UNDECLARED: Self = Self(None);

    /// `true` when the id this slot came from is declared by the policy.
    /// Test-only.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn is_declared(self) -> bool {
        self.0.is_some()
    }
}

/// Collection of named [`TokenBucket`]s, built from a [`Policy`]'s
/// `rate_limits` table.
///
/// Owned by the [`PolicyEngine`][pe]. Handler-side call sites call
/// [`Self::try_consume`] with the same string id referenced by the matched
/// rule's `rate_limit` field. Unknown ids return `true` (fail-open on
/// lookup — the engine is still fail-closed overall because an unmatched
/// sequence falls through to `defaults.unmatched`).
///
/// The buckets live in a `Vec` and the ids index into it, so a hot call site
/// can resolve its id to a [`RateLimitSlot`] once and pay an index instead of
/// a string hash per dispatch. `try_consume(id)` is exactly
/// `try_consume_slot(slot(id))` and keeps every documented semantic,
/// including "unknown id ⇒ allow".
///
/// [pe]: crate::engine::PolicyEngine
#[derive(Debug, Clone, Default)]
pub struct RateLimiterSet {
    /// One bucket per DISTINCT declared id, in first-declaration order.
    buckets: Vec<TokenBucket>,
    /// Declared id → its index in `buckets`. Consulted by the string-keyed
    /// entry points and by [`Self::slot`]; never on a resolved-slot debit.
    slots: HashMap<String, usize>,
}

impl RateLimiterSet {
    /// Build the set from a [`Policy`]'s `rate_limits` table.
    ///
    /// Duplicate ids in the policy are silently collapsed — the last
    /// occurrence wins. This matches TOML's own semantics for repeated
    /// `[[rate_limits]]` tables and keeps the builder total. A duplicate
    /// OVERWRITES the first occurrence's bucket in place, so it neither adds
    /// a slot nor moves any other id's slot.
    #[must_use]
    pub fn from_policy(policy: &Policy) -> Self {
        // Capacity is only a pre-allocation hint (both containers grow as
        // needed), so clamping it bounds the up-front allocation the verifier
        // must budget for without changing behavior — real policies declare a
        // handful of rate limits, nowhere near the clamp.
        let cap = policy.rate_limits.len().min(1024);
        let mut buckets = Vec::with_capacity(cap);
        let mut slots = HashMap::with_capacity(cap);
        for cfg in &policy.rate_limits {
            let bucket = TokenBucket::from_config(cfg);
            match slots.get(&cfg.id) {
                // Last occurrence wins, in the first occurrence's slot.
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

    /// Resolve a rate-limit id to its bucket slot, for call sites that debit
    /// the same id many times under one policy.
    ///
    /// Returns [`RateLimitSlot::UNDECLARED`] when this policy declares no
    /// bucket with that id; debiting that slot allows, exactly as passing the
    /// unknown id to [`Self::try_consume`] would.
    #[must_use]
    #[inline]
    pub fn slot(&self, id: &str) -> RateLimitSlot {
        RateLimitSlot(self.slots.get(id).copied())
    }

    /// Attempt to debit `amount` tokens from the bucket named by `id`.
    ///
    /// Returns:
    ///
    /// * `true` if the id is known and the bucket had capacity.
    /// * `true` if the id is unknown (no declared limit ⇒ unlimited).
    /// * `false` if the id is known but the bucket denied the request.
    ///
    /// The "unknown id ⇒ allow" branch is deliberate: the engine's
    /// fail-closed posture is enforced at rule-evaluation time via
    /// `defaults.unmatched`. Rate limits only narrow *matched* rules.
    #[inline]
    pub fn try_consume<T: TimeSource>(&mut self, id: &str, amount: u64, clock: &T) -> bool {
        let slot = self.slot(id);
        self.try_consume_slot(slot, amount, clock)
    }

    /// Attempt to debit `amount` tokens from the bucket a [`RateLimitSlot`]
    /// names — the string-hash-free form of [`Self::try_consume`].
    ///
    /// Same three outcomes, with [`RateLimitSlot::UNDECLARED`] standing in for
    /// "unknown id ⇒ allow". A slot issued by a DIFFERENT set whose index is
    /// past this set's buckets also allows, so the operation stays total (see
    /// the [`RateLimitSlot`] docs on why a caller must not rely on that).
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

    // -----------------------------------------------------------------
    // TokenBucket semantics
    // -----------------------------------------------------------------

    #[test]
    fn burst_capacity_drains_then_denies() {
        // 200-byte burst, 100 B/s refill, no time advance: 5 × 40 fits,
        // remaining 5 must be denied.
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
        // 100 B capacity, 10_000 B/s refill — idle 10 s should not
        // accumulate past 100.
        let mut bucket = TokenBucket::from_config(&rl("x", 100, 10_000, 0));
        let clock = FakeClock::new();

        assert!(bucket.try_consume(100, &clock));
        clock.advance(Duration::from_secs(10));
        assert!(bucket.try_consume(100, &clock));
        assert!(!bucket.try_consume(1, &clock));
    }

    #[test]
    fn oversized_request_always_denied_without_draining() {
        // A single call larger than capacity must deny *without* debiting
        // any well-behaved callers' balance.
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

    // -----------------------------------------------------------------
    // per_sequence_max semantics
    // -----------------------------------------------------------------

    #[test]
    fn per_sequence_cap_rejects_oversized_call() {
        // capacity=1000 so ordinary rate-limit checks pass, but
        // per_sequence_max=16 must reject amount=17.
        let mut bucket = TokenBucket::from_config(&rl("palette", 1_000, 1_000, 16));
        let clock = FakeClock::new();

        assert!(bucket.try_consume(16, &clock));
        assert!(!bucket.try_consume(17, &clock));
        // A 16-count call is still permitted after the denial since the
        // denial did not debit the bucket.
        assert!(bucket.try_consume(16, &clock));
    }

    #[test]
    fn per_sequence_cap_zero_disables_check() {
        // per_sequence_max=0 means "disabled"; only capacity/refill apply.
        let mut bucket = TokenBucket::from_config(&rl("response", 65_536, 102_400, 0));
        let clock = FakeClock::new();
        // A single 64 KiB response is permitted (matches capacity).
        assert!(bucket.try_consume(65_536, &clock));
        // The next byte fails on balance, not on per-sequence cap.
        assert!(!bucket.try_consume(1, &clock));
    }

    // -----------------------------------------------------------------
    // RateLimiterSet
    // -----------------------------------------------------------------

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
        // Unknown ids still allowed.
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
        // The collapse must not have created a second slot: a duplicate
        // overwrites the first occurrence's bucket rather than pushing.
        assert_eq!(set.len(), 1);
    }

    // -----------------------------------------------------------------
    // RateLimitSlot — the pre-resolved form of the same debit
    // -----------------------------------------------------------------

    fn two_bucket_policy() -> Policy {
        Policy {
            schema_version: crate::SCHEMA_VERSION,
            profile: crate::Profile::Standard,
            defaults: crate::Defaults {
                unmatched: crate::Response::Drop,
                shell_integration_require_nonce: true,
            },
            rules: vec![],
            // Two buckets so a slot mix-up is observable: debiting the wrong
            // one would leave the other's balance untouched.
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
        // The "response" bucket is now empty and "palette" is untouched: a
        // slot that pointed at the wrong bucket fails one of these.
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
        // Same fail-open answer the string form gives, at any amount.
        assert!(set.try_consume_slot(missing, 99_999, &clock));
        assert!(set.try_consume("no-such-bucket", 99_999, &clock));
        // The default slot is the undeclared one, not bucket 0.
        assert_eq!(RateLimitSlot::default(), RateLimitSlot::UNDECLARED);
    }

    #[test]
    fn slot_from_a_wider_set_stays_total() {
        // A slot resolved against a set with more buckets is out of range
        // here. It must allow (like an unknown id), never panic and never
        // alias another bucket.
        let wide = RateLimiterSet::from_policy(&two_bucket_policy());
        let stale = wide.slot("palette");
        let narrow_policy = Policy {
            rate_limits: vec![rl("response", 100, 1_000, 0)],
            ..two_bucket_policy()
        };
        let mut narrow = RateLimiterSet::from_policy(&narrow_policy);
        let clock = FakeClock::new();
        assert!(narrow.try_consume_slot(stale, 99_999, &clock));
        // "response" is still full — the stale slot debited nothing.
        assert!(narrow.try_consume("response", 100, &clock));
    }
}
