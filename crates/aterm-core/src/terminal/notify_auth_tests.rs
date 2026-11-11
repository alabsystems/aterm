// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use std::sync::Arc;

fn pace(policy: &mut PolicyState) -> NotificationPace<'_> {
    at(policy, aterm_time::Instant::now())
}

fn at(policy: &mut PolicyState, now: aterm_time::Instant) -> NotificationPace<'_> {
    NotificationPace {
        policy,
        clock: BatchClock(now),
    }
}

#[test]
fn nothing_mints_by_default_or_while_notifications_are_off() {
    let mut policy = PolicyState::new();
    let mut sinks = NotificationSinks::new();
    assert!(
        sinks.try_mint(true, pace(&mut policy)).is_none(),
        "no callback"
    );
    assert!(
        sinks.try_mint_advanced(true, pace(&mut policy)).is_none(),
        "no callback"
    );
    sinks.set_callback(Box::new(|_| {}));
    sinks.set_advanced_callback(Box::new(|_| {}));
    assert!(sinks.try_mint(false, pace(&mut policy)).is_none());
    assert!(sinks.try_mint_advanced(false, pace(&mut policy)).is_none());
    assert!(sinks.try_mint(true, pace(&mut policy)).is_some());
    assert!(sinks.try_mint_advanced(true, pace(&mut policy)).is_some());
}

#[test]
fn each_callback_mints_its_own_capability() {
    let mut policy = PolicyState::new();
    let mut sinks = NotificationSinks::new();
    sinks.set_advanced_callback(Box::new(|_| {}));
    assert!(sinks.try_mint(true, pace(&mut policy)).is_none());
    assert!(sinks.try_mint_advanced(true, pace(&mut policy)).is_some());
}

#[test]
fn a_minted_capability_reaches_its_callback() {
    let mut policy = PolicyState::new();
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mut sinks = NotificationSinks::new();
    let into = Arc::clone(&seen);
    sinks.set_callback(Box::new(move |m| into.lock().unwrap().push(m.to_string())));
    let into = Arc::clone(&seen);
    sinks.set_advanced_callback(Box::new(move |n| {
        into.lock().unwrap().push(n.title.unwrap_or_default());
    }));
    let cap = sinks.try_mint(true, pace(&mut policy)).expect("minted");
    sinks.notify(cap, "simple");
    let cap = sinks
        .try_mint_advanced(true, pace(&mut policy))
        .expect("minted");
    let notification = Notification {
        title: Some("advanced".into()),
        ..Notification::default()
    };
    sinks.notify_advanced(cap, notification);
    assert_eq!(*seen.lock().unwrap(), ["simple", "advanced"]);
}

/// A flood mints its burst, then one a second: the terminal's own pace with no policy, the
/// standard policy's `notifications` bucket with it.
#[test]
fn a_flood_is_paced_to_its_burst_then_one_a_second() {
    use aterm_policy::engine::PolicyEngine;
    let t0 = aterm_time::Instant::now();
    let second = std::time::Duration::from_secs(1);
    let mut own = PolicyState::new();
    let mut standard = PolicyState::new();
    standard.install(PolicyEngine::new(aterm_policy::profiles::standard()));
    for policy in [&mut own, &mut standard] {
        let mut sinks = NotificationSinks::new();
        sinks.set_callback(Box::new(|_| {}));
        sinks.set_advanced_callback(Box::new(|_| {}));
        let minted = (0..100)
            .filter(|i| {
                if i % 2 == 0 {
                    sinks.try_mint(true, at(policy, t0)).is_some()
                } else {
                    sinks.try_mint_advanced(true, at(policy, t0)).is_some()
                }
            })
            .count();
        assert_eq!(minted as u64, PACE_BURST, "both callbacks share one pace");
        assert!(sinks.try_mint(true, at(policy, t0 + second / 2)).is_none());
        assert!(sinks.try_mint(true, at(policy, t0 + second)).is_some());
        assert!(sinks.try_mint(true, at(policy, t0 + second)).is_none());
    }
}
