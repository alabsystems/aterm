// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Structural capability gate on the OSC 9, OSC 99 and OSC 777 notification callbacks.
//!
//! The callbacks and the terminal's own notification pace live in [`NotificationSinks`], whose
//! fields only this module reads: a notification reaches the host only through
//! [`NotificationSinks::notify`] or [`NotificationSinks::notify_advanced`], each taking the
//! capability its mint returns when `allow_notifications` is on, that callback is installed, and
//! it fits the terminal's notification pace ([`NotificationPace`]).

use super::callbacks::{AdvancedNotificationCallback, NotificationCallback};
use super::policy_gates::PolicyState;
use super::response_rate_limiter::{BatchClock, ResponseRateLimiter};
use aterm_types::osc::Notification;

/// Program notifications a terminal raises at once before its pace holds them to
/// [`PACE_PER_SECOND`], when no installed policy declares a `notifications` limit: the standard
/// profile's numbers.
const PACE_BURST: u64 = 16;

/// Program notifications a second a terminal raises once its [`PACE_BURST`] is spent.
const PACE_PER_SECOND: u64 = 1;

/// What one program notification's pace is debited against: the installed policy's
/// `notifications` bucket when it declares one, else the terminal's own, at the batch's clock.
pub(super) struct NotificationPace<'a> {
    pub(super) policy: &'a mut PolicyState,
    pub(super) clock: BatchClock,
}

/// Proof that an OSC 9 notification may reach the simple callback, minted only by
/// [`NotificationSinks::try_mint`].
#[must_use = "NotificationCapability has no effect unless passed to notify"]
pub(super) struct NotificationCapability {
    _seal: super::Seal,
}

/// Proof that an OSC 99 or OSC 777 notification may reach the advanced callback, minted only by
/// [`NotificationSinks::try_mint_advanced`].
#[must_use = "AdvancedNotificationCapability has no effect unless passed to notify_advanced"]
pub(super) struct AdvancedNotificationCapability {
    _seal: super::Seal,
}

/// The notification callbacks and the terminal's own notification pace.
pub(super) struct NotificationSinks {
    callback: Option<NotificationCallback>,
    advanced_callback: Option<AdvancedNotificationCallback>,
    /// The pace when no installed policy declares a `notifications` limit; a reset keeps it, so
    /// a program cannot refill it by resetting the terminal.
    pace: ResponseRateLimiter,
}

impl NotificationSinks {
    #[inline]
    pub(super) const fn new() -> Self {
        Self {
            callback: None,
            advanced_callback: None,
            pace: ResponseRateLimiter::with_limits(PACE_PER_SECOND, PACE_BURST),
        }
    }

    #[inline]
    pub(super) fn set_callback(&mut self, callback: NotificationCallback) {
        self.callback = Some(callback);
    }

    #[inline]
    pub(super) fn set_advanced_callback(&mut self, callback: AdvancedNotificationCallback) {
        self.advanced_callback = Some(callback);
    }

    /// Whether one more notification fits `pace`, debiting it when it does.
    fn paced(&mut self, pace: NotificationPace<'_>) -> bool {
        let NotificationPace { policy, clock } = pace;
        policy
            .notification_limit_try_consume(&clock)
            .unwrap_or_else(|| self.pace.try_consume(1, &clock))
    }

    /// Mint an OSC 9 capability when `allowed` (`allow_notifications`), the simple callback is
    /// installed and the notification fits `pace`; the pace is debited only once both of the
    /// first hold.
    #[inline]
    pub(super) fn try_mint(
        &mut self,
        allowed: bool,
        pace: NotificationPace<'_>,
    ) -> Option<NotificationCapability> {
        (allowed && self.callback.is_some() && self.paced(pace))
            .then_some(NotificationCapability { _seal: super::Seal })
    }

    /// Mint an OSC 99 or OSC 777 capability: the rules of [`Self::try_mint`] against the advanced
    /// callback.
    #[inline]
    pub(super) fn try_mint_advanced(
        &mut self,
        allowed: bool,
        pace: NotificationPace<'_>,
    ) -> Option<AdvancedNotificationCapability> {
        (allowed && self.advanced_callback.is_some() && self.paced(pace))
            .then_some(AdvancedNotificationCapability { _seal: super::Seal })
    }

    /// Hand `message` to the simple callback, consuming the capability.
    #[inline]
    pub(super) fn notify(&mut self, _token: NotificationCapability, message: &str) {
        if let Some(ref mut callback) = self.callback {
            callback(message);
        }
    }

    /// Hand `notification` to the advanced callback, consuming the capability.
    #[inline]
    pub(super) fn notify_advanced(
        &mut self,
        _token: AdvancedNotificationCapability,
        notification: Notification,
    ) {
        if let Some(ref mut callback) = self.advanced_callback {
            callback(notification);
        }
    }
}

#[cfg(test)]
mod tests {
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
}
