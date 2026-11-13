// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn slots_refuse_at_the_cap_and_take_back_what_drops() {
    let slots = Arc::new(Slots::new(2));
    let first = slots.try_take().expect("below the cap");
    let second = slots.try_take().expect("below the cap");
    assert!(slots.try_take().is_none(), "the cap refuses a third");
    assert_eq!(slots.used(), 2, "a refusal takes nothing");
    let over = slots.take_uncapped();
    assert_eq!(slots.used(), 3, "an uncapped slot is still counted");
    drop((first, over));
    assert_eq!(slots.used(), 1);
    let again = slots.try_take().expect("a dropped slot is free again");
    drop((second, again));
    assert_eq!(slots.used(), 0);
}
