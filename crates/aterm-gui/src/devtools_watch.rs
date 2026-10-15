// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE DEVELOPER TOOLS LIST, HANDLED BY THE WINDOW (owner ruling 2026-10-14: "make sure
//! that this is handled automatically in aterm batteries included").
//!
//! macOS holds every new program aterm starts for its first-run check unless aterm is on
//! System Settings ▸ Privacy & Security ▸ Developer Tools (`atpkg::devtools`). Only the
//! owner can switch it on (macOS asks for their password), so what the window does is
//! bring the owner to the switch, once:
//!
//! * it reads the list at its first focus — a launch, and so the first look of a build an
//!   update just brought in — and again when the owner comes back to aterm while aterm is
//!   not on it, at most once a minute ([`REREAD_EVERY`]; the read is the `privacy` verb's:
//!   the switch's row in the system TCC.db and tccd's answers, on a worker,
//!   `Wake::DevToolsProbeReady` when done);
//! * `not_granted` raises ONE digest line whose one press opens the pane
//!   ([`crate::message_reporters::devtools_entry`]), and, once per aterm version, opens the
//!   pane itself ([`atpkg::devtools::hand_to_owner`], with `spctl developer-mode
//!   enable-terminal` first on a Mac whose list is empty, so the pane exists);
//! * `granted` ends the line; `unknown` changes nothing.
//!
//! It never presses anything in System Settings, never writes TCC.db and never calls
//! tccutil: decided 2026-10-14 against driving the pane through Accessibility (design
//! §9.1, O19). A headless instance, and every unit test, reads nothing and opens nothing:
//! the consent arms there are inert and the handler returns before it acts.

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
    /// aterm is not on the list: raise the line, and open the pane for the owner when
    /// `hand_over` (not yet done for this version).
    Raise {
        /// Open the pane (once per aterm version).
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
    /// This process already asked for the pane opened: two readings that land before
    /// the worker writes its marker open it once.
    handed_over: bool,
}

impl DevToolsWatch {
    /// Whether a focus gain at `now` should read the list: the first one (the launch
    /// read), then, while aterm is not on the list, one at most every [`REREAD_EVERY`].
    pub(crate) fn wants_read(&mut self, now: Instant) -> bool {
        let want = !self.read_once
            || (self.awaiting_owner
                && self
                    .last_read
                    .is_none_or(|at| now.saturating_duration_since(at) >= REREAD_EVERY));
        self.read_once = true;
        if want {
            self.last_read = Some(now);
        }
        want
    }

    /// What a completed reading asks for. `opened_this_version`: aterm already opened
    /// the pane for the running version (`atpkg::devtools::last_opened`).
    pub(crate) fn observe(&mut self, grant: DevToolGrant, opened_this_version: bool) -> Step {
        match grant {
            DevToolGrant::Granted => {
                self.awaiting_owner = false;
                Step::Clear
            }
            DevToolGrant::NotGranted => {
                self.awaiting_owner = true;
                let hand_over = !opened_this_version && !self.handed_over;
                self.handed_over |= hand_over;
                Step::Raise { hand_over }
            }
            DevToolGrant::Unknown => Step::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The launch read once; then reads only while the owner is wanted; the pane opens
    /// once per version; `unknown` never moves anything.
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
    }
}
