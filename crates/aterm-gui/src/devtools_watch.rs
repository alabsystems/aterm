// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Window-driven watcher for the macOS Developer Tools privacy list.

use std::time::{Duration, Instant};

use aterm_containment::DevToolGrant;

/// Minimum interval between focus-triggered re-reads while aterm is not on the list.
pub(crate) const REREAD_EVERY: Duration = Duration::from_secs(60);

/// What one reading asks of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing changes.
    Nothing,
    /// Raises the digest line and optionally requests bringing up the Settings pane.
    Raise {
        /// Ask for the pane brought up (once per aterm version).
        hand_over: bool,
    },
    /// aterm is on the list: end the line.
    Clear,
}

/// When the window reads the list, and what a reading asks of it.
#[derive(Debug, Default)]
pub(crate) struct DevToolsWatch {
    /// The launch read was asked for.
    read_once: bool,
    /// Whether the last reading was `NotGranted`.
    awaiting_owner: bool,
    /// When a focus gain last asked for a read.
    last_read: Option<Instant>,
    /// Whether this process already requested opening the Settings pane.
    handed_over: bool,
    /// Whether the next focus gain should re-read immediately after a Settings hand-over.
    read_on_return: bool,
    /// Whether a transition to `Granted` owes a macOS verdict refresh.
    refresh_owed: bool,
    /// Whether the current switch-on already requested a refresh.
    refreshed: bool,
    /// Whether a hand-over return read is currently in flight.
    prime_on_return: bool,
}

impl DevToolsWatch {
    /// Returns whether a focus gain at `now` should read the Developer Tools list.
    pub(crate) fn wants_read(&mut self, now: Instant) -> bool {
        let want = !self.read_once
            || self.read_on_return
            || (self.awaiting_owner
                && self
                    .last_read
                    .is_none_or(|at| now.saturating_duration_since(at) >= REREAD_EVERY));
        self.read_once = true;
        if want {
            self.last_read = Some(now);
            self.prime_on_return |= self.read_on_return;
            self.read_on_return = false;
        }
        want
    }

    /// Records that aterm opened the Developer Tools pane so the next focus gain re-reads.
    pub(crate) const fn note_handed_over(&mut self) {
        self.read_on_return = true;
    }

    /// Resets the hand-over latch after an inconclusive helper attempt.
    pub(crate) const fn forget_hand_over(&mut self) {
        self.handed_over = false;
    }

    /// Folds a completed `grant` reading into the watcher and returns the window [`Step`].
    pub(crate) fn observe(&mut self, grant: DevToolGrant, opened_this_version: bool) -> Step {
        match grant {
            DevToolGrant::Granted => {
                if self.awaiting_owner {
                    self.owe_refresh();
                }
                self.awaiting_owner = false;
                Step::Clear
            }
            DevToolGrant::NotGranted => {
                self.awaiting_owner = true;
                self.refreshed = false;
                let hand_over = !opened_this_version && !self.handed_over;
                self.handed_over |= hand_over;
                Step::Raise { hand_over }
            }
            DevToolGrant::Unknown => Step::Nothing,
        }
    }

    /// Records that the owner confirmed switching on Developer Tools and requests a refresh.
    pub(crate) const fn note_switched_on(&mut self) -> bool {
        self.refreshed = true;
        self.refresh_owed = false;
        true
    }

    /// Takes whether the owner's return reading requires refreshing macOS's verdict.
    pub(crate) fn take_return_prime(&mut self, list_read: bool, grant: DevToolGrant) -> bool {
        let owed = std::mem::take(&mut self.prime_on_return);
        let prime = owed && !list_read && grant != DevToolGrant::Granted;
        if prime {
            self.refreshed = true;
            self.refresh_owed = false;
        }
        prime
    }

    /// Takes whether a verdict refresh is owed to macOS.
    pub(crate) fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut self.refresh_owed)
    }

    const fn owe_refresh(&mut self) {
        if !self.refreshed {
            self.refreshed = true;
            self.refresh_owed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads once at launch, re-reads only while `NotGranted`, and hands over once per version.
    #[test]
    fn the_watch_reads_at_launch_and_while_the_owner_is_wanted() {
        let t0 = Instant::now();
        let mut watch = DevToolsWatch::default();
        assert!(watch.wants_read(t0), "the launch read");
        assert!(!watch.wants_read(t0), "one launch read, not one per focus");
        assert_eq!(watch.observe(DevToolGrant::Unknown, false), Step::Nothing);
        assert!(!watch.wants_read(t0 + REREAD_EVERY * 9));
        assert_eq!(
            watch.observe(DevToolGrant::NotGranted, false),
            Step::Raise { hand_over: true }
        );
        let t1 = t0 + REREAD_EVERY;
        assert!(watch.wants_read(t1), "the owner coming back reads again");
        assert!(
            !watch.wants_read(t1 + REREAD_EVERY / 2),
            "not on every focus"
        );
        assert!(watch.wants_read(t1 + REREAD_EVERY));
        assert_eq!(
            watch.observe(DevToolGrant::NotGranted, false),
            Step::Raise { hand_over: false },
            "a second reading before the marker is written opens nothing"
        );
        assert_eq!(
            watch.observe(DevToolGrant::NotGranted, true),
            Step::Raise { hand_over: false },
            "opened once for this version"
        );
        assert_eq!(watch.observe(DevToolGrant::Granted, true), Step::Clear);
        assert!(
            !watch.wants_read(t1 + REREAD_EVERY * 9),
            "on the list: focus reads nothing"
        );
        watch.forget_hand_over();
        assert_eq!(
            watch.observe(DevToolGrant::NotGranted, false),
            Step::Raise { hand_over: true }
        );
        assert_eq!(
            watch.observe(DevToolGrant::NotGranted, false),
            Step::Raise { hand_over: false }
        );
    }

    /// Returning from a Settings hand-over bypasses the cooldown for a single read.
    #[test]
    fn the_return_from_a_hand_over_reads_however_soon_it_comes() {
        let t0 = Instant::now();
        let soon = Duration::from_secs(20);
        assert!(soon < REREAD_EVERY);
        let mut watch = DevToolsWatch::default();
        assert!(watch.wants_read(t0));
        assert_eq!(
            watch.observe(DevToolGrant::NotGranted, false),
            Step::Raise { hand_over: true }
        );
        assert!(!watch.wants_read(t0 + soon), "no hand-over: the gate holds");
        let mut watch = DevToolsWatch::default();
        assert!(watch.wants_read(t0));
        assert_eq!(
            watch.observe(DevToolGrant::NotGranted, false),
            Step::Raise { hand_over: true }
        );
        watch.note_handed_over();
        assert!(watch.wants_read(t0 + soon), "back from System Settings");
        assert!(
            !watch.wants_read(t0 + soon + Duration::from_secs(1)),
            "one read: the gate is back"
        );
        assert!(watch.wants_read(t0 + soon + REREAD_EVERY));
        for grant in [DevToolGrant::Unknown, DevToolGrant::Granted] {
            let mut watch = DevToolsWatch::default();
            assert!(watch.wants_read(t0));
            let _ = watch.observe(grant, false);
            assert!(!watch.wants_read(t0 + REREAD_EVERY * 9), "{grant:?}");
            watch.note_handed_over();
            assert!(watch.wants_read(t0 + REREAD_EVERY * 9), "{grant:?}");
            assert!(!watch.wants_read(t0 + REREAD_EVERY * 20), "{grant:?}");
        }
    }

    /// Requests a verdict refresh once per `NotGranted`-to-`Granted` transition or manual confirm.
    #[test]
    fn macos_is_asked_about_aterm_again_once_per_switch_on() {
        let mut watch = DevToolsWatch::default();
        let _ = watch.observe(DevToolGrant::Granted, false);
        assert!(!watch.take_refresh(), "granted at launch: never refused");

        let mut watch = DevToolsWatch::default();
        let _ = watch.observe(DevToolGrant::NotGranted, true);
        assert!(!watch.take_refresh());
        let _ = watch.observe(DevToolGrant::Unknown, true);
        assert!(!watch.take_refresh(), "unknown moves nothing");
        let _ = watch.observe(DevToolGrant::Granted, true);
        assert!(watch.take_refresh(), "switched on");
        assert!(!watch.take_refresh(), "taken");
        let _ = watch.observe(DevToolGrant::Granted, true);
        assert!(!watch.take_refresh(), "still on: no second ask");
        let _ = watch.observe(DevToolGrant::NotGranted, true);
        let _ = watch.observe(DevToolGrant::Granted, true);
        assert!(watch.take_refresh(), "off and on again: asked again");

        let mut watch = DevToolsWatch::default();
        let _ = watch.observe(DevToolGrant::NotGranted, true);
        assert!(watch.note_switched_on(), "the owner's press");
        assert!(
            watch.note_switched_on(),
            "every press: an early one met the switch still off"
        );
        let _ = watch.observe(DevToolGrant::Granted, true);
        assert!(
            !watch.take_refresh(),
            "the reading the press brought about asks no more"
        );
    }

    /// Returning from a hand-over primes once when the list cannot be read directly.
    #[test]
    fn the_owners_return_asks_once_when_the_list_cannot_be_read() {
        let t0 = Instant::now();
        let mut watch = DevToolsWatch::default();
        assert!(watch.wants_read(t0), "the launch read");
        let _ = watch.observe(DevToolGrant::NotGranted, true);
        assert!(
            !watch.take_return_prime(false, DevToolGrant::NotGranted),
            "the launch read is no return"
        );
        watch.note_handed_over();
        assert!(watch.wants_read(t0), "the owner's return reads");
        let _ = watch.observe(DevToolGrant::NotGranted, true);
        assert!(watch.take_return_prime(false, DevToolGrant::NotGranted));
        assert!(
            !watch.take_return_prime(false, DevToolGrant::NotGranted),
            "one per return"
        );
        let _ = watch.observe(DevToolGrant::Granted, true);
        assert!(
            !watch.take_refresh(),
            "the reading it brings about asks no more"
        );
        for (list_read, grant) in [
            (true, DevToolGrant::NotGranted),
            (false, DevToolGrant::Granted),
        ] {
            watch.note_handed_over();
            assert!(watch.wants_read(t0));
            assert!(
                !watch.take_return_prime(list_read, grant),
                "{list_read} {grant:?}"
            );
        }
    }
}
