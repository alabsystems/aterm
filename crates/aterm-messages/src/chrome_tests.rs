// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn no_rise_is_no_floor_and_every_fall_is_allowed() {
    let now = Instant::now();
    let f = RegrowFloor::default();
    assert_eq!(f.last_rise(), None);
    assert_eq!(f.floor(), None);
    assert!(fall_allowed(f.floor(), now));
    assert_eq!(at_floor(now, f.floor()), now);
}

#[test]
fn a_fall_waits_the_settle_past_the_last_rise() {
    let t0 = Instant::now();
    let mut f = RegrowFloor::default();
    f.rose(t0);
    assert_eq!(f.floor(), Some(t0 + CHROME_REGROW_SETTLE));
    assert!(!fall_allowed(f.floor(), t0));
    assert!(!fall_allowed(f.floor(), t0 + ms(5)));
    assert!(!fall_allowed(f.floor(), t0 + CHROME_REGROW_SETTLE - ms(1)));
    assert!(fall_allowed(f.floor(), t0 + CHROME_REGROW_SETTLE));
    assert_eq!(at_floor(t0 + ms(6), f.floor()), t0 + CHROME_REGROW_SETTLE);
    assert_eq!(at_floor(t0 + ms(900), f.floor()), t0 + ms(900));
}

#[test]
fn the_floor_counts_from_the_latest_rise_and_never_moves_back() {
    let t0 = Instant::now();
    let mut f = RegrowFloor::default();
    f.rose(t0);
    f.rose(t0 + ms(300));
    assert_eq!(f.last_rise(), Some(t0 + ms(300)));
    assert!(!fall_allowed(f.floor(), t0 + CHROME_REGROW_SETTLE));
    f.rose(t0 + ms(100));
    assert_eq!(f.last_rise(), Some(t0 + ms(300)), "a stale rise");
    assert!(fall_allowed(f.floor(), t0 + ms(700)));
}
