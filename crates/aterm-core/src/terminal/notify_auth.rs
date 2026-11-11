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
#[path = "notify_auth_tests.rs"]
mod tests;
