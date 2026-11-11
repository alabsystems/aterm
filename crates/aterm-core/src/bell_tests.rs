// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn ring_activates_for_flash_duration() {
    let mut flash = BellFlash::new();
    let t0 = Instant::now();
    flash.ring(t0);
    assert!(flash.is_active(t0));
    assert!(flash.is_active(t0 + FLASH_DURATION - Duration::from_millis(1)));
    assert!(!flash.is_active(t0 + FLASH_DURATION));
    assert_eq!(flash.deadline(), Some(t0 + FLASH_DURATION));
}

#[test]
fn expire_clears_only_after_deadline() {
    let mut flash = BellFlash::new();
    let t0 = Instant::now();
    flash.ring(t0);
    assert!(!flash.expire(t0 + Duration::from_millis(50)));
    assert!(flash.is_active(t0 + Duration::from_millis(50)));
    let end = t0 + FLASH_DURATION;
    assert!(flash.expire(end));
    assert!(flash.deadline().is_none());
    assert!(!flash.is_active(end));
    assert!(!flash.expire(end));
}

#[test]
fn ring_during_flash_extends_deadline() {
    let mut flash = BellFlash::new();
    let t0 = Instant::now();
    flash.ring(t0);
    let t1 = t0 + Duration::from_millis(60);
    flash.ring(t1);
    assert_eq!(flash.deadline(), Some(t1 + FLASH_DURATION));
    assert!(flash.is_active(t0 + FLASH_DURATION + Duration::from_millis(10)));
}

#[test]
fn rate_limiter_blocks_within_interval() {
    let mut gate = BellRateLimiter::new(Duration::from_secs(1));
    let t0 = Instant::now();
    assert!(gate.try_fire(t0));
    assert!(!gate.try_fire(t0));
    assert!(!gate.try_fire(t0 + Duration::from_millis(999)));
}

#[test]
fn rate_limiter_allows_at_interval_boundary() {
    let mut gate = BellRateLimiter::new(Duration::from_secs(1));
    let t0 = Instant::now();
    assert!(gate.try_fire(t0));
    assert!(gate.try_fire(t0 + Duration::from_secs(1)));
    assert!(!gate.try_fire(t0 + Duration::from_millis(1500)));
    assert!(gate.try_fire(t0 + Duration::from_secs(2)));
}
