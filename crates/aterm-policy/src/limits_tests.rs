// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

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

/// NEGATIVE CONTROL: `restore_snapshot` clamps non-finite (`NaN`, `±∞`), negative, and `-0.0`
/// balances to `+0.0` and over-capacity balances to `capacity_bytes`, while `earliest_refill`
/// and `PolicyEngine` snapshot helpers round-trip declared buckets in first-declaration order.
#[test]
fn snapshot_restore_clamps_invalid_balances_and_tracks_earliest_refill() {
    let mut bucket = TokenBucket::from_config(&rl("x", 100, 10, 0));
    assert_eq!(
        bucket.snapshot(),
        BucketSnapshot {
            tokens: 100.0,
            last_refill: None
        }
    );

    let clock = FakeClock::new();
    let t0 = clock.now();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -25.0, -0.0] {
        bucket.restore_snapshot(BucketSnapshot {
            tokens: bad,
            last_refill: Some(t0),
        });
        let snap = bucket.snapshot();
        assert_eq!(snap.tokens.to_bits(), 0.0_f64.to_bits(), "bad={bad}");
        assert_eq!(snap.last_refill, Some(t0));
    }

    bucket.restore_snapshot(BucketSnapshot {
        tokens: 250.0,
        last_refill: None,
    });
    assert_eq!(
        bucket.snapshot(),
        BucketSnapshot {
            tokens: 100.0,
            last_refill: None
        }
    );

    let mut engine = crate::engine::PolicyEngine::new(two_bucket_policy());
    assert_eq!(engine.earliest_refill(), None);
    assert!(engine.rate_limit_try_consume("palette", 4, &clock));
    assert_eq!(engine.earliest_refill(), Some(t0));

    clock.advance(Duration::from_millis(50));
    let t1 = clock.now();
    assert!(engine.rate_limit_try_consume("response", 30, &clock));
    assert_eq!(engine.earliest_refill(), Some(t0));

    let saved = engine.bucket_snapshots();
    assert_eq!(
        saved,
        vec![
            BucketSnapshot {
                tokens: 70.0,
                last_refill: Some(t1)
            },
            BucketSnapshot {
                tokens: 6.0,
                last_refill: Some(t0)
            },
        ]
    );

    let mut restored = crate::engine::PolicyEngine::new(two_bucket_policy());
    restored.restore_bucket_snapshots(&saved[..1]);
    assert_eq!(
        restored.bucket_snapshots(),
        vec![
            saved[0],
            BucketSnapshot {
                tokens: 10.0,
                last_refill: None
            },
        ]
    );
    restored.restore_bucket_snapshots(&saved);
    assert_eq!(restored.bucket_snapshots(), saved);
    assert_eq!(restored.earliest_refill(), Some(t0));
}
