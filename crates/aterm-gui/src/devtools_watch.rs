// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Developer Tools list, handled by the window.
//!
//! macOS holds every new program aterm starts for its first-run check unless aterm is on
//! System Settings ▸ Privacy & Security ▸ Developer Tools (`atpkg::devtools`). Only the
//! owner can switch it on, so the window brings them to the switch:
//!
//! * it reads the list at its first focus, and again when the owner comes back to aterm
//!   while aterm is not on it, at most every [`REREAD_EVERY`] (the `privacy` verb's read,
//!   on a worker, `Wake::DevToolsProbeReady` when done); the first focus after aterm
//!   brought the owner to the pane reads however soon it comes
//!   ([`DevToolsWatch::note_handed_over`]);
//! * `not_granted` raises one digest line whose press opens the pane and whose second
//!   capsule opens aterm's Settings ▸ Security
//!   ([`crate::message_reporters::devtools_entry`]), and once per aterm version asks for
//!   the pane brought up at a quiet moment (`App::hand_devtools_to_owner`,
//!   [`crate::devtools_helper`]);
//! * `granted` ends the line; `unknown` changes nothing.
//!
//! It never presses anything in System Settings, never writes TCC.db and never calls
//! tccutil. A headless instance, and every unit test, reads nothing and opens nothing.

use std::time::{Duration, Instant};

use aterm_containment::DevToolGrant;

/// While aterm is not on the list, a focus gain reads it again at most this often: one
/// read is a `log show` of about a second, and an owner who declines still switches
/// windows all day.
pub(crate) const REREAD_EVERY: Duration = Duration::from_secs(60);

/// What one reading asks of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing changes.
    Nothing,
    /// aterm is not on the list: raise the line, and when `hand_over` ask for the pane
    /// brought up for the owner.
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
    /// The last reading said `not_granted`: the owner may be off switching it on, so
    /// coming back to aterm reads again.
    awaiting_owner: bool,
    /// When a focus gain last asked for a read.
    last_read: Option<Instant>,
    /// This process already asked for the pane brought up: two readings that land
    /// before the hand-over writes its marker ask once.
    handed_over: bool,
    /// aterm brought the owner to the pane since the last read: the next focus gain reads
    /// regardless of `last_read` and `awaiting_owner`.
    read_on_return: bool,
    /// macOS should be asked about aterm again ([`Self::take_refresh`]): a reading said
    /// `not_granted` and a later one `granted`, or the owner said they switched it on.
    refresh_owed: bool,
    /// The ask for this switch-on was made: readings that keep saying `granted`, the
    /// Settings card's every two minutes among them, ask no more. The next
    /// `not_granted` reading re-arms it.
    refreshed: bool,
    /// The owner's return read is out ([`Self::take_return_prime`]): the focus gain
    /// after aterm brought them to the page asked for it.
    prime_on_return: bool,
}

impl DevToolsWatch {
    /// Whether a focus gain at `now` should read the list: the first one (the launch
    /// read); the first after aterm brought the owner to the pane
    /// ([`Self::note_handed_over`]); otherwise, while aterm is not on the list, one at
    /// most every [`REREAD_EVERY`].
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

    /// aterm just brought the owner to the Developer Tools pane: the next focus gain reads
    /// the list, even within [`REREAD_EVERY`] of the last read (the launch read is seconds
    /// old when the first focus hands the pane over) and whatever it said. One read, then
    /// the gate is back.
    pub(crate) fn note_handed_over(&mut self) {
        self.read_on_return = true;
    }

    /// The window's own hand-over was refused as `unknown` or `listed`, which decides
    /// nothing against bringing the owner to the page: the next `not_granted` reading
    /// asks again, once.
    pub(crate) fn forget_hand_over(&mut self) {
        self.handed_over = false;
    }

    /// What a completed reading asks for. `opened_this_version`: aterm already opened
    /// the pane for the running version (`atpkg::devtools::last_opened`).
    pub(crate) fn observe(&mut self, grant: DevToolGrant, opened_this_version: bool) -> Step {
        match grant {
            DevToolGrant::Granted => {
                // Only a process that read `not_granted` was checked before the switch
                // went on, so only it holds a verdict macOS must be asked to redo; one
                // already granted at its launch read was never refused.
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

    /// The owner pressed "I've switched it on" (the panel aterm cannot confirm from,
    /// its list unreadable without Full Disk Access): macOS is asked about aterm again
    /// at once — every press, since one made before the password was typed met the
    /// switch still off — and its answer is then the reading that confirms the press.
    /// The `granted` reading it brings about asks no more.
    pub(crate) fn note_switched_on(&mut self) -> bool {
        self.refreshed = true;
        self.refresh_owed = false;
        true
    }

    /// Whether the reading the owner's return brought ([`Self::note_handed_over`],
    /// then [`Self::wants_read`]) asks macOS about aterm again: when it could not read
    /// the list (`list_read` false: no Full Disk Access) and tccd has not answered
    /// `granted`. Without the list, a process checked before the switch went on never
    /// reads `granted` by itself — tccd is not asked about it again — so whatever the
    /// owner did on the page (a press before their password, or none), coming back from
    /// it asks once. Taken by the first completed reading after that focus gain.
    pub(crate) fn take_return_prime(&mut self, list_read: bool, grant: DevToolGrant) -> bool {
        let owed = std::mem::take(&mut self.prime_on_return);
        let prime = owed && !list_read && grant != DevToolGrant::Granted;
        if prime {
            self.refreshed = true;
            self.refresh_owed = false;
        }
        prime
    }

    /// Whether macOS is owed an ask about aterm now (`atpkg::devtools::refresh_verdict`):
    /// once per switch-on, and the answer is taken.
    pub(crate) fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut self.refresh_owed)
    }

    fn owe_refresh(&mut self) {
        if !self.refreshed {
            self.refreshed = true;
            self.refresh_owed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The launch read once; then reads only while the owner is wanted; the hand-over is
    /// asked once per version; `unknown` never moves anything.
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
        // A refused hand-over, forgotten, asks again at the next `not_granted`, once;
        // unforgotten (above) it asked once.
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

    /// After aterm brings the owner to the pane, the focus gain that brings them back
    /// reads, though the launch read was seconds before; one read, then the gate is back.
    /// It reads whatever the last reading said. Negative control: the same return with no
    /// hand-over reads nothing.
    #[test]
    fn the_return_from_a_hand_over_reads_however_soon_it_comes() {
        let t0 = Instant::now();
        let soon = Duration::from_secs(20);
        assert!(soon < REREAD_EVERY);
        // The control: launch read, not granted, back 20 s later: gated.
        let mut watch = DevToolsWatch::default();
        assert!(watch.wants_read(t0));
        assert_eq!(
            watch.observe(DevToolGrant::NotGranted, false),
            Step::Raise { hand_over: true }
        );
        assert!(!watch.wants_read(t0 + soon), "no hand-over: the gate holds");
        // The same, with the pane handed over.
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
        // Not awaiting the owner (the list could not be read, or read on): an owner's
        // press still has its return read.
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

    /// MACOS IS ASKED ABOUT ATERM AGAIN once per switch-on, and only by a process it
    /// checked before the switch went on: `not_granted` then `granted` owes one ask, and
    /// the `granted` readings after it (the Settings card's every two minutes) none.
    /// NEGATIVE CONTROLS: a launch already `granted` was never refused and owes none;
    /// `unknown` owes none. Switched off and on again owes one more. The owner's "I've
    /// switched it on" owes one, and the `granted` reading it brings about no second.
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

    /// THE OWNER'S RETURN ASKS ONCE when aterm cannot read the list: a process checked
    /// before the switch went on never reads `granted` by itself there, so the reading
    /// the return from the page brings asks macOS again whatever happened on it.
    /// NEGATIVE CONTROLS: no return without a hand-over; a list read (Full Disk Access)
    /// or a `granted` answer asks nothing; one ask per return.
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
