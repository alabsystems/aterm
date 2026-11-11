// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn t0() -> Instant {
    Instant::now()
}

#[test]
fn envelope_rises_holds_and_expires() {
    let mut d = DogCameo::default();
    let t = t0();
    assert_eq!(d.alpha(t), 0, "no dog before the first summon");
    assert!(!d.active(t));
    d.on_summon(t, 7);
    assert!(d.active(t));
    assert_eq!(d.alpha(t + FADE_IN), 255, "fully present after the fade-in");
    assert_eq!(d.alpha(t + FADE_IN + HOLD - Duration::from_millis(1)), 255);
    let gone = t + FADE_IN + HOLD + FADE_OUT;
    assert_eq!(d.alpha(gone), 0, "expired after the fade-out");
    assert!(!d.active(gone));
    assert!(
        d.look().is_some(),
        "the identity survives expiry for a mid-fade frame's benefit"
    );
}

#[test]
fn resummons_parade_distinct_breeds() {
    let t = t0();
    let mut d = DogCameo::default();
    d.on_summon(t, 1);
    let first = d.look().unwrap().breed;
    for seed in 0..32u64 {
        let mut fresh = DogCameo::default();
        fresh.on_summon(t, 1);
        fresh.on_summon(t + Duration::from_millis(300), seed);
        assert_ne!(
            fresh.look().unwrap().breed,
            first,
            "a visible dog is never re-rolled onto itself (seed {seed})"
        );
    }
}

#[test]
fn entry_bounce_hops_then_settles() {
    let mut d = DogCameo::default();
    let t = t0();
    d.on_summon(t, 3);
    let peak1 = d.bob(t + Duration::from_millis(HOP_MS / 2));
    let peak2 = d.bob(t + Duration::from_millis(HOP_MS + HOP_MS / 2));
    assert!(peak1 < 0.0, "a hop lifts the dog");
    assert!(peak2 < 0.0 && peak2 > peak1, "the second hop is lower");
    assert_eq!(
        d.bob(t + Duration::from_millis(HOP_MS * HOPS + 1)),
        0.0,
        "settled after the hops"
    );
    assert_eq!(DogCameo::default().bob(t), 0.0, "no bounce before a summon");
}
