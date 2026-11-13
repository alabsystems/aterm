// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Window-driven watcher for the macOS Developer Tools privacy list.

use std::time::{Duration, Instant};

use aterm_containment::DevToolGrant;

/// Minimum interval between focus-triggered re-reads while aterm is not on the list.
pub(crate) const REREAD_EVERY: Duration = Duration::from_secs(60);

/// A read owed an approval or grant retries even when a worker could not start.
const OWED_READ_RETRY: Duration = Duration::from_secs(1);

/// What one reading asks of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing changes.
    Nothing,
    /// aterm is not on the list: when `hand_over`, ask for the pane brought up for the owner.
    NotOn {
        /// Ask for the pane brought up (once per approved revision).
        hand_over: bool,
    },
    /// aterm is on the list.
    Clear,
}

/// When the window reads the list, and what a reading asks of it.
#[derive(Debug, Default)]
pub(crate) struct DevToolsWatch {
    /// The launch read was asked for.
    read_once: bool,
    /// A read is owed once the probe is on again ([`Self::hold_read`]); the window makes it then
    /// (`App::resume_held_launch_steps`), whatever else the gate says.
    read_held: bool,
    /// Whether the last reading was `NotGranted`.
    awaiting_owner: bool,
    /// When a focus gain last asked for a read.
    last_read: Option<Instant>,
    /// Whether this process already requested opening the Settings pane.
    handed_over: bool,
    /// Whether the next focus gain should re-read immediately after a Settings hand-over.
    read_on_return: bool,
    /// Full Disk Access is held ([`Self::note_fda_granted`]): the list reads, so an `Unknown`
    /// reading no longer means it cannot be read.
    fda_granted: bool,
    /// A grant or new approval owes a read until its current completion is observed.
    read_owed: bool,
    /// The approval whose fresh reading keeps the new-revision page's recent-open deferral.
    read_revision: Option<u64>,
    /// The last reading was `Unknown` while Full Disk Access was held: re-read as for `NotGranted`.
    unknown_with_fda: bool,
    /// Whether a transition to `Granted` owes a macOS verdict refresh.
    refresh_owed: bool,
    /// Whether the current switch-on already requested a refresh.
    refreshed: bool,
    /// Whether a hand-over return read is currently in flight.
    prime_on_return: bool,
    /// A verdict refresh waits for macOS's program check to catch up ([`Self::hold_prime`]).
    prime_held: bool,
}

impl DevToolsWatch {
    /// Returns whether a focus gain at `now` should read the Developer Tools list.
    pub(crate) fn wants_read(&mut self, now: Instant) -> bool {
        let want = self.due(now);
        self.read_held = false;
        self.read_once = true;
        if want {
            self.last_read = Some(now);
            self.prime_on_return |= self.read_on_return;
            self.read_on_return = false;
        }
        want
    }

    /// Whether a focus gain at `now` reads ([`Self::wants_read`]), spending nothing.
    pub(crate) fn due(&self, now: Instant) -> bool {
        self.read_held
            || !self.read_once
            || self.read_on_return
            || self.read_owed
            || ((self.awaiting_owner || self.unknown_with_fda)
                && self
                    .last_read
                    .is_none_or(|at| now.saturating_duration_since(at) >= REREAD_EVERY))
    }

    /// The probe could read nothing for a due read: a focus gain came while `[privacy] enabled` or
    /// `check` was off (no read spent; the launch read stays owed), or a switch went off before a
    /// read's completion was handled (the read spent on nothing). Either way the first park with
    /// every switch on and a window focused, or the next focus gain, reads. A switch off is a
    /// pause, never an answer.
    pub(crate) fn hold_read(&mut self) {
        self.read_held = true;
    }

    /// Whether a focus gain's read waits for the probe to be on again.
    pub(crate) const fn read_held(&self) -> bool {
        self.read_held
    }

    pub(crate) const fn owes_read(&self) -> bool {
        self.read_held() || self.read_owed
    }

    /// Keep a refused dispatch or retired in-flight completion from spending this read.
    pub(crate) fn read_deadline(&self, now: Instant) -> Option<Instant> {
        self.read_owed
            .then(|| self.last_read.map_or(now, |at| at + OWED_READ_RETRY))
    }

    /// A new approval must read this Mac now, even after the last one read Granted.
    pub(crate) fn read_for_revision(&mut self, revision: u64) {
        self.read_revision = Some(revision);
        self.read_owed = true;
        self.last_read = None;
        self.forget_hand_over();
    }

    pub(crate) const fn reading_revision(&self) -> Option<u64> {
        self.read_revision
    }

    pub(crate) fn take_reading_revision(&mut self) -> Option<u64> {
        self.read_revision.take()
    }

    /// Records that aterm opened the Developer Tools pane so the next focus gain re-reads.
    pub(crate) const fn note_handed_over(&mut self) {
        self.read_on_return = true;
    }

    /// Full Disk Access reads granted: the list can be read now, so one read is owed at once (the
    /// window makes it without waiting for a focus gain), once per process, and stays owed until
    /// a reading is observed. Unlike [`Self::note_handed_over`] it owes no verdict refresh:
    /// nothing was handed over.
    pub(crate) const fn note_fda_granted(&mut self) {
        if !self.fda_granted {
            self.fda_granted = true;
            self.read_owed = true;
        }
    }

    /// Resets the hand-over latch after an inconclusive helper attempt or a newly approved access
    /// revision.
    pub(crate) const fn forget_hand_over(&mut self) {
        self.handed_over = false;
    }

    /// What a completed reading asks for.
    pub(crate) fn observe(&mut self, grant: DevToolGrant, opened_this_revision: bool) -> Step {
        // A panel's independent observation does not consume a new approval's slot read.
        if self.read_revision.is_none() {
            self.read_owed = false;
        }
        self.unknown_with_fda = grant == DevToolGrant::Unknown && self.fda_granted;
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
                let hand_over = !opened_this_revision && !self.handed_over;
                self.handed_over |= hand_over;
                Step::NotOn { hand_over }
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

    /// Holds a verdict refresh until a view stands in no episode of macOS's program check: one asked
    /// for while the check is backed up (its prime would wait in the same line), one that did not
    /// settle, or one that did not start.
    pub(crate) const fn hold_prime(&mut self) {
        self.prime_held = true;
    }

    /// Takes whether a held verdict refresh waits ([`Self::hold_prime`]).
    pub(crate) fn take_held_prime(&mut self) -> bool {
        std::mem::take(&mut self.prime_held)
    }

    const fn owe_refresh(&mut self) {
        if !self.refreshed {
            self.refreshed = true;
            self.refresh_owed = true;
        }
    }
}

#[cfg(test)]
#[path = "devtools_watch_tests.rs"]
mod tests;
