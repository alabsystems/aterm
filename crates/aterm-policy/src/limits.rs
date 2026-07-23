// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Canonical token-bucket rate limiter for policy [`RateLimit`] entries.
//!
//! Part of #7995. Hosts the single source-of-truth implementation consumed
//! by the [`PolicyEngine`][pe]. The engine owns one [`TokenBucket`] per
//! [`RateLimit`][rl] id declared on the active [`Policy`][p].
//!
//! # Semantics
//!
//! A [`TokenBucket`] enforces three independent limits that map 1-to-1 onto
//! the fields of [`RateLimit`][rl]:
//!
//! * `capacity_bytes` — burst budget. Tokens accumulate up to this ceiling
//!   while idle and drain by `amount` on every successful consume.
//! * `refill_per_second` — steady-state rate. Tokens replenish linearly
//!   between calls. A rate of 0 freezes the bucket at the current balance.
//! * `per_sequence_max` — hard cap on any single `try_consume(amount)` call,
//!   independent of the current token balance. A value of 0 disables the
//!   per-call cap (only steady-state + burst apply). This models the
//!   existing `MAX_RESPONSES_PER_SEQUENCE` counter in the OSC 4 / OSC 21
//!   palette handlers (#7883).
//!
//! A bucket with `capacity_bytes = 0` is a hard "deny all" — useful as a
//! kill switch from a hot-swapped policy.
//!
//! # Time source abstraction
//!
//! Production callers use [`SystemClock`] (a zero-size wrapper over
//! [`Instant::now`]). Tests inject a [`FakeClock`]-like type implementing
//! [`TimeSource`] so the clock can advance deterministically without
//! sleeping. This is the same pattern the pre-#7995 `response_rate_limiter`
//! module used in aterm-core; we preserve it verbatim so the migration is
//! value-equivalent.
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
//! These properties are exercised by the Kani harness `rate_limit_bounds`
//! (§8.1 of the design) and by the unit-test suite in this module.
//!
//! [pe]: crate::engine::PolicyEngine
//! [rl]: crate::RateLimit
//! [p]: crate::Policy

use std::collections::HashMap;
use std::time::Duration;
// web_time::Instant == std::time::Instant on native; JS clock on wasm32 (engine-wide
// web_time discipline — std::time::Instant::now() panics on wasm).
use web_time::Instant;

use crate::{Policy, RateLimit};

// ---------------------------------------------------------------------------
// TimeSource abstraction
// ---------------------------------------------------------------------------

/// Abstraction over [`Instant::now`] so tests can drive the clock forward
/// deterministically without sleeping.
///
/// Production callers use [`SystemClock`]; tests build a stateful fake that
/// advances only when explicitly stepped.
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

// ---------------------------------------------------------------------------
// TokenBucket
// ---------------------------------------------------------------------------

/// Token-bucket state for one named [`RateLimit`] entry.
///
/// Built from a [`RateLimit`] configuration via [`Self::from_config`]. The
/// bucket starts full (balance == `capacity_bytes`) so the first
/// `try_consume` call never spuriously denies at startup.
///
/// All knobs (`capacity_bytes`, `refill_per_second`, `per_sequence_max`)
/// are captured as `u64` to keep the arithmetic regular; the on-wire
/// schema uses `u32` (see [`RateLimit`]) which widens losslessly.
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
    /// Last refill instant. `None` means "uninitialized — seed on next
    /// call" so the first `try_consume` never pays a time lookup during
    /// construction.
    last_refill: Option<Instant>,
}

impl TokenBucket {
    /// Construct a bucket from a [`RateLimit`] configuration.
    ///
    /// The bucket starts at full capacity.
    #[must_use]
    pub fn from_config(cfg: &RateLimit) -> Self {
        let capacity_bytes = u64::from(cfg.capacity_bytes);
        Self {
            #[allow(
                clippy::cast_precision_loss,
                reason = "capacity is bytes-scale; f64 has >40 bits of precision for any realistic cap"
            )]
            tokens: capacity_bytes as f64,
            capacity_bytes,
            refill_per_second: u64::from(cfg.refill_per_second),
            per_sequence_max: u64::from(cfg.per_sequence_max),
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
        // Case 1: hard block.
        if self.capacity_bytes == 0 {
            return false;
        }
        // Case 2: per-call cap. `per_sequence_max == 0` means "disabled".
        if self.per_sequence_max > 0 && amount > self.per_sequence_max {
            return false;
        }
        // Case 3: oversized. An amount larger than capacity cannot fit
        // even with a full bucket; drop without debiting.
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

    /// Reconfigure the bucket in place from a new [`RateLimit`].
    ///
    /// The current balance is preserved but clamped down to the new
    /// capacity so a host that shrinks the ceiling cannot be immediately
    /// over-budget.
    pub fn reconfigure(&mut self, cfg: &RateLimit) {
        self.capacity_bytes = u64::from(cfg.capacity_bytes);
        self.refill_per_second = u64::from(cfg.refill_per_second);
        self.per_sequence_max = u64::from(cfg.per_sequence_max);
        #[allow(
            clippy::cast_precision_loss,
            reason = "capacity is bytes-scale; f64 precision sufficient"
        )]
        let cap = self.capacity_bytes as f64;
        if self.tokens > cap {
            self.tokens = cap;
        }
    }

    /// Current burst capacity (read-only accessor; intended for tests
    /// and diagnostics).
    #[must_use]
    pub fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes
    }

    /// Current token balance rounded down to whole tokens (read-only
    /// accessor; intended for tests and diagnostics).
    #[must_use]
    pub fn tokens(&self) -> u64 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "tokens are clamped to [0, capacity] which fits in u64"
        )]
        let t = self.tokens as u64;
        t
    }

    /// Per-sequence cap, or `0` when disabled.
    #[must_use]
    pub fn per_sequence_max(&self) -> u64 {
        self.per_sequence_max
    }
}

// ---------------------------------------------------------------------------
// RateLimiterSet
// ---------------------------------------------------------------------------

/// Collection of named [`TokenBucket`]s, built from a [`Policy`]'s
/// `rate_limits` table.
///
/// Owned by the [`PolicyEngine`][pe]. Handler-side call sites call
/// [`Self::try_consume`] with the same string id referenced by the matched
/// rule's `rate_limit` field. Unknown ids return `true` (fail-open on
/// lookup — the engine is still fail-closed overall because an unmatched
/// sequence falls through to `defaults.unmatched`).
///
/// [pe]: crate::engine::PolicyEngine
#[derive(Debug, Clone, Default)]
pub struct RateLimiterSet {
    buckets: HashMap<String, TokenBucket>,
}

impl RateLimiterSet {
    /// Construct an empty set (no buckets). Equivalent to a policy with
    /// an empty `rate_limits` table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the set from a [`Policy`]'s `rate_limits` table.
    ///
    /// Duplicate ids in the policy are silently collapsed — the last
    /// occurrence wins. This matches TOML's own semantics for repeated
    /// `[[rate_limits]]` tables and keeps the builder total.
    #[must_use]
    pub fn from_policy(policy: &Policy) -> Self {
        // Capacity is only a pre-allocation hint (the map grows as needed), so
        // clamping it bounds the up-front allocation the verifier must budget
        // for without changing behavior — real policies declare a handful of
        // rate limits, nowhere near the clamp.
        let cap = policy.rate_limits.len().min(1024);
        let mut buckets = HashMap::with_capacity(cap);
        for cfg in &policy.rate_limits {
            buckets.insert(cfg.id.clone(), TokenBucket::from_config(cfg));
        }
        Self { buckets }
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
    pub fn try_consume<T: TimeSource>(&mut self, id: &str, amount: u64, clock: &T) -> bool {
        match self.buckets.get_mut(id) {
            Some(bucket) => bucket.try_consume(amount, clock),
            None => true,
        }
    }

    /// Borrow a bucket by id — useful for diagnostics. Returns `None`
    /// when no bucket with that id has been declared by the policy.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&TokenBucket> {
        self.buckets.get(id)
    }

    /// Borrow a bucket mutably by id. Returns `None` when no bucket
    /// with that id has been declared. Intended for host-side
    /// reconfigure paths.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut TokenBucket> {
        self.buckets.get_mut(id)
    }

    /// `true` if a bucket with the given id exists.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.buckets.contains_key(id)
    }

    /// Number of buckets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// `true` when no buckets are declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

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
    // TokenBucket semantics — mirror the aterm-core/response_rate_limiter
    // tests so the migration is value-equivalent.
    // -----------------------------------------------------------------

    #[test]
    fn below_rate_all_calls_succeed() {
        let mut bucket = TokenBucket::from_config(&rl("x", 500, 1_000, 0));
        let clock = FakeClock::new();
        for _ in 0..10 {
            assert!(bucket.try_consume(40, &clock));
            clock.advance(Duration::from_millis(200));
        }
    }

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

    #[test]
    fn reconfigure_preserves_tokens_under_new_cap() {
        let mut bucket = TokenBucket::from_config(&rl("x", 500, 1_000, 0));
        let clock = FakeClock::new();
        assert!(bucket.try_consume(200, &clock));
        assert_eq!(bucket.tokens(), 300);

        bucket.reconfigure(&rl("x", 100, 1_000, 0));
        assert_eq!(bucket.capacity_bytes(), 100);
        assert!(bucket.tokens() <= 100);
    }

    // -----------------------------------------------------------------
    // per_sequence_max semantics (ports #7883 MAX_RESPONSES_PER_SEQUENCE)
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

    #[test]
    fn per_sequence_cap_does_not_debit_on_denial() {
        let mut bucket = TokenBucket::from_config(&rl("palette", 1_000, 1_000, 4));
        let clock = FakeClock::new();
        // 10 pathological calls at amount=5 (each above the per-seq cap)
        // — every one denied, no tokens consumed.
        for _ in 0..10 {
            assert!(!bucket.try_consume(5, &clock));
        }
        // Bucket is still full.
        assert_eq!(bucket.tokens(), 1_000);
    }

    // -----------------------------------------------------------------
    // RateLimiterSet
    // -----------------------------------------------------------------

    #[test]
    fn unknown_id_is_allowed_by_default() {
        let mut set = RateLimiterSet::new();
        let clock = FakeClock::new();
        // No bucket declared → unmatched ids are permitted. The engine
        // is still fail-closed overall because unmatched *rules* fall
        // through to defaults.unmatched.
        assert!(set.try_consume("missing", 99_999, &clock));
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
    }
}
