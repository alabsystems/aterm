// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Structural capability gate on the OSC 9, OSC 99 and OSC 777 notification callbacks.
//!
//! The callbacks and the host's admission live in [`NotificationSinks`], whose fields only this
//! module reads: a notification reaches the host only through [`NotificationSinks::notify`] or
//! [`NotificationSinks::notify_advanced`], each taking the capability its mint returns when
//! `allow_notifications` is on, that callback is installed, and the admission admits it.

use super::callbacks::{AdvancedNotificationCallback, NotificationCallback};
use super::program_state::ProgramStateReport;
use aterm_types::osc::Notification;

/// The host's admission for one program notification, asked with the program's own state report
/// as it stands at the notification's byte ([`super::Terminal::set_notification_admission`]).
pub(super) type NotificationAdmission = Box<dyn FnMut(&ProgramStateReport) -> bool + Send>;

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

/// The notification callbacks and the host's admission.
pub(super) struct NotificationSinks {
    callback: Option<NotificationCallback>,
    advanced_callback: Option<AdvancedNotificationCallback>,
    admission: Option<NotificationAdmission>,
}

impl NotificationSinks {
    #[inline]
    pub(super) const fn new() -> Self {
        Self {
            callback: None,
            advanced_callback: None,
            admission: None,
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

    #[inline]
    pub(super) fn set_admission(&mut self, admit: NotificationAdmission) {
        self.admission = Some(admit);
    }

    /// Whether the host admits a notification written while `report` stands; none admits all.
    fn admits(&mut self, report: &ProgramStateReport) -> bool {
        self.admission.as_mut().is_none_or(|admit| admit(report))
    }

    /// Mint an OSC 9 capability when `allowed` (`allow_notifications`), the simple callback is
    /// installed and the admission admits; the admission is asked only once both hold.
    #[inline]
    pub(super) fn try_mint(
        &mut self,
        allowed: bool,
        report: &ProgramStateReport,
    ) -> Option<NotificationCapability> {
        (allowed && self.callback.is_some() && self.admits(report))
            .then_some(NotificationCapability { _seal: super::Seal })
    }

    /// Mint an OSC 99 or OSC 777 capability: the rules of [`Self::try_mint`] against the advanced
    /// callback.
    #[inline]
    pub(super) fn try_mint_advanced(
        &mut self,
        allowed: bool,
        report: &ProgramStateReport,
    ) -> Option<AdvancedNotificationCapability> {
        (allowed && self.advanced_callback.is_some() && self.admits(report))
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting_admission(asked: &Arc<AtomicUsize>, verdict: bool) -> NotificationAdmission {
        let asked = Arc::clone(asked);
        Box::new(move |_| {
            asked.fetch_add(1, Ordering::Relaxed);
            verdict
        })
    }

    #[test]
    fn nothing_mints_by_default_or_while_notifications_are_off() {
        let report = ProgramStateReport::default();
        let mut sinks = NotificationSinks::new();
        assert!(sinks.try_mint(true, &report).is_none(), "no callback");
        assert!(
            sinks.try_mint_advanced(true, &report).is_none(),
            "no callback"
        );
        sinks.set_callback(Box::new(|_| {}));
        sinks.set_advanced_callback(Box::new(|_| {}));
        assert!(sinks.try_mint(false, &report).is_none());
        assert!(sinks.try_mint_advanced(false, &report).is_none());
        assert!(sinks.try_mint(true, &report).is_some());
        assert!(sinks.try_mint_advanced(true, &report).is_some());
    }

    #[test]
    fn each_callback_mints_its_own_capability() {
        let report = ProgramStateReport::default();
        let mut sinks = NotificationSinks::new();
        sinks.set_advanced_callback(Box::new(|_| {}));
        assert!(sinks.try_mint(true, &report).is_none());
        assert!(sinks.try_mint_advanced(true, &report).is_some());
    }

    #[test]
    fn the_admission_is_asked_only_where_a_callback_would_fire_and_its_no_mints_nothing() {
        let report = ProgramStateReport::default();
        let asked = Arc::new(AtomicUsize::new(0));
        let mut sinks = NotificationSinks::new();
        sinks.set_admission(counting_admission(&asked, false));
        assert!(sinks.try_mint(true, &report).is_none());
        sinks.set_callback(Box::new(|_| {}));
        assert!(sinks.try_mint(false, &report).is_none());
        assert_eq!(
            asked.load(Ordering::Relaxed),
            0,
            "asked with no callback or while off"
        );
        assert!(
            sinks.try_mint(true, &report).is_none(),
            "refused by the admission"
        );
        assert_eq!(asked.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_minted_capability_reaches_its_callback() {
        let report = ProgramStateReport::default();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut sinks = NotificationSinks::new();
        let into = Arc::clone(&seen);
        sinks.set_callback(Box::new(move |m| into.lock().unwrap().push(m.to_string())));
        let into = Arc::clone(&seen);
        sinks.set_advanced_callback(Box::new(move |n| {
            into.lock().unwrap().push(n.title.unwrap_or_default());
        }));
        let cap = sinks.try_mint(true, &report).expect("minted");
        sinks.notify(cap, "simple");
        let cap = sinks.try_mint_advanced(true, &report).expect("minted");
        let notification = Notification {
            title: Some("advanced".into()),
            ..Notification::default()
        };
        sinks.notify_advanced(cap, notification);
        assert_eq!(*seen.lock().unwrap(), ["simple", "advanced"]);
    }
}
