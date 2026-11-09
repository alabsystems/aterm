// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What holds the automatic update lane once its ladder says `Land`, and how each hold is named.
//!
//! Past [`crate::native_update_auto_intent::LANDS_WITHIN`] the ladder no longer waits for the
//! terminal, so a poll that still does not attempt is held by something else: a hold. Every hold
//! the lane honours is a [`LaneHold`], and every one states its bound ([`LaneHold::bound`]): it
//! lifts within a fixed time, at an event this program makes by itself, or, for a person's own
//! state (a dialog up waiting on them, a draft they typed), only while that state is true. None
//! waits for a window to take an event: a hold that did (the System State offer of 0.146.0,
//! answered only once a person was at the screen) kept an owner's staged update waiting 3.45 days
//! for a touch.
//!
//! The lane names the hold it is under where a person and a program can read it ([`NamedHold`],
//! kept in the armed intent so it never outlives it): one info line when a hold begins or
//! changes (never one per poll), one when it ends, and `apply_held=<token>` in
//! `aterm ctl update status`.

use std::time::{Duration, Instant};

/// Defines [`LaneHold`] and its wire tokens from one list, so [`LaneHold::ALL`] and
/// [`LaneHold::token`] cannot leave a hold out.
macro_rules! lane_holds {
    ($($(#[$doc:meta])* $hold:ident => $token:literal,)+) => {
        /// One hold of the automatic lane (`apply_held=` spells it [`Self::token`]). Off macOS
        /// the handoff that meets some of these does not run, so they are never made there.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        pub(crate) enum LaneHold {
            $($(#[$doc])* $hold,)+
        }

        impl LaneHold {
            /// Every hold, as the list defines them.
            #[cfg(test)]
            pub(crate) const ALL: &'static [Self] = &[$(Self::$hold,)+];

            /// The wire spelling (`aterm ctl update status` prints it as `apply_held=`).
            #[must_use]
            pub(crate) const fn token(self) -> &'static str {
                match self {
                    $(Self::$hold => $token,)+
                }
            }
        }
    };
}

lane_holds! {
    /// The updater's own check, download or stage reconcile is running (`NativeUpdaterService`'s
    /// active work); every request it makes is time-bound.
    UpdaterBusy => "updater-busy",
    /// An update handoff is already in flight, or the updater is applying; it commits or stands
    /// down within [`crate::app_update_handoff_boundary::HANDOFF_PENDING_BOUND`].
    HandoffInFlight => "handoff-in-flight",
    /// A person's Install is held for the candidate's pre-park read, which ends by itself.
    PersonInstall => "person-install",
    /// The stage on disk is not yet the exact armed artifact (a download or reconcile settling).
    StagePending => "stage-pending",
    /// The installed build is being replaced by a verified newer build (`native_activation_supersede`).
    ActivationSupersede => "activation-superseded",
    /// A folder-access warm-up the person started may have a macOS consent dialog up; capped by
    /// `[privacy] warmup_hold_ms`, at most [`crate::app_config::PRIVACY_WARMUP_HOLD_MS_MAX`].
    ConsentWarmup => "consent-warmup",
    /// A cold restore's agents are still being relaunched; at most
    /// [`crate::harness_host::RESTORED_HOLD`] from the first relaunch, once per process.
    RestoredAgents => "restored-agents",
    /// The park's capture refused this desk; tried again when the desk changes, on its spacing, and
    /// at the latest after [`crate::app_native::CAPTURE_REFUSAL_BACKSTOP`], an attempt that ends
    /// the hold.
    CaptureRefusal => "capture-refused",
    /// The stage's signer waits for the next update check to confirm it, which the checker runs
    /// by itself on its cadence; its wait says its own line (`App::hold_for_roster_confirmation`).
    RosterConfirmation => "signer-unconfirmed",
    /// The candidate's pre-park read of exactly these bytes is running.
    PreparkRead => "prepark-read",
    /// A launch-record renewal's handoff is in flight; the update follows it.
    LaunchRecordRenewal => "launch-record-renewal",
    /// An item that cannot be cut in half (a sign-out's clicks, a publish rename, a launchd
    /// bootstrap) is in its critical section, which is aterm's own work: each try follows it for
    /// at most [`crate::app_update_handoff_boundary::MAX_BOUNDARY_WAIT`], and the next try follows
    /// it again, so the hold lasts as long as the item's section does.
    BoundaryItem => "boundary-item",
    /// A launch's Mac setup offer has a person's draft in it (a group pressed, a vault address or
    /// password typed), which the successor could not carry. It is the person's own and holds only
    /// while it is there; an offer nobody touched holds nothing, whatever the setup worker is doing
    /// (`SystemStateApp::offer_hold`).
    SetupDraft => "setup-draft",
    /// A launch's Mac setup offer is being answered by the page itself: its own answer, a queued
    /// page action or a Stop is under way, which the page settles by itself at its next ticks.
    SetupAnswer => "setup-answer",
    /// A System State Apply, Restore or queued choice is still pending: the setup worker answers
    /// it, or the other setup holding this Mac's records finishes.
    SystemState => "system-state",
    /// A Command Line Tools install's own tab is still running and cannot travel with the
    /// handoff (its password prompt waiting on the person, or a child the successor could not
    /// recover): it holds only while that tab's program runs.
    DeveloperTools => "developer-tools",
    /// A sudo password prompt is open, waiting on the person: it holds only while it is open.
    PasswordPrompt => "password-prompt",
    /// A new tab is still starting; a spawn is bounded by `aterm_pty::START_HANG`.
    TabStarting => "tab-starting",
    /// Input or PTY output inside the quiet epoch, before the reader park; only an `Automatic`
    /// (before-grace) attempt reads it, so it never holds past `Land`.
    Activity => "activity",
    /// A handed session's command exited while its pane is still open; its reader sees the
    /// hang-up and retires it.
    PaneExited => "pane-exited",
    /// A live `video` take holds the park; only before `Land`.
    Recording => "recording",
    /// The target build's handoff policy is being read before the fork lane parks under it.
    PolicyRead => "policy-read",
    /// The terminal got busy mid-attempt (or a park missed its rung): retried on the activity
    /// spacing, 2 s rising to at most 30 s, never exhausted; each revocation says its own line
    /// with its retry.
    ActivityRevoked => "activity-revoked",
}

/// A person's own state that is true now, the only thing that may hold the lane on a person
/// ([`HoldBound::PersonNow`]): a dialog up waiting on them, or a draft they typed and have not
/// applied. Each is one [`LaneHold`] ([`Self::hold`]), and holds only while it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonHold {
    /// A sudo password prompt is open, waiting on them.
    PasswordPrompt,
    /// A Command Line Tools install's tab that cannot travel with the handoff, its password
    /// prompt waiting on them.
    DeveloperTools,
    /// Their draft in the launch's setup offer.
    SetupDraft,
}

impl PersonHold {
    /// The lane hold this person's state is.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn hold(self) -> LaneHold {
        match self {
            Self::PasswordPrompt => LaneHold::PasswordPrompt,
            Self::DeveloperTools => LaneHold::DeveloperTools,
            Self::SetupDraft => LaneHold::SetupDraft,
        }
    }

    /// The bound as the line says it: whose state it is, and that it holds only while it is.
    #[must_use]
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::PasswordPrompt => " (a person's own prompt: it holds only while it is open)",
            Self::DeveloperTools => " (a person's own install tab: it holds only while it runs)",
            Self::SetupDraft => " (a person's own draft: it holds only while it is there)",
        }
    }
}

/// How a hold is bounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldBound {
    /// It lifts within this long of its start, whatever anyone does.
    Within(Duration),
    /// It lifts at an event this program makes by itself: a worker's answer, a timer, a check,
    /// the end of aterm's own work.
    Program,
    /// A person's own state that is true now; it holds only while that state is, never on how
    /// long ago a window took an event or whether anyone is at the Mac.
    PersonNow(PersonHold),
}

impl LaneHold {
    /// What lifts the hold, for the log line that names it ("it lands when …").
    #[must_use]
    pub(crate) const fn lifts(self) -> &'static str {
        match self {
            Self::UpdaterBusy => "the updater's running check or download ends",
            Self::HandoffInFlight => "the handoff in flight commits or stands down",
            Self::PersonInstall => "the candidate's pre-park read ends",
            Self::StagePending => "the staged download settles as the armed artifact",
            Self::ActivationSupersede => "the verified newer build replaces the installed one",
            Self::ConsentWarmup => "the folder-access warm-up ends",
            Self::RestoredAgents => "the restored agents are relaunched",
            Self::CaptureRefusal => "the desk changes or the refusal's spacing passes",
            Self::RosterConfirmation => "the next update check confirms the stage's signer",
            Self::PreparkRead => "the pre-park read of these bytes ends",
            Self::LaunchRecordRenewal => "the launch-record renewal's handoff ends",
            Self::BoundaryItem => "the item leaves its critical section",
            Self::SetupDraft => "the person's draft in the setup offer is applied or cleared",
            Self::SetupAnswer => {
                "the setup page settles its own answer, its queued action or its Stop"
            }
            Self::SystemState => {
                "the setup worker answers the pending Apply or Restore, or the other setup \
                 holding this Mac's records finishes"
            }
            Self::DeveloperTools => "the Command Line Tools install's tab ends",
            Self::PasswordPrompt => "the sudo password prompt is answered or closed",
            Self::TabStarting => "the new tab has started",
            Self::Activity => "a quiet epoch passes",
            Self::PaneExited => "the exited pane's reader retires it",
            Self::Recording => "the video take ends or the ladder reaches land",
            Self::PolicyRead => "the target build's handoff policy is read",
            Self::ActivityRevoked => "a retry reaches its Commit with no activity in between",
        }
    }

    /// How the hold is bounded. No arm may wait for a person's event: a hold is bounded by time,
    /// by this program, or by a person's state that is true now, which only a [`PersonHold`]
    /// names.
    #[must_use]
    pub(crate) const fn bound(self) -> HoldBound {
        match self {
            Self::HandoffInFlight => {
                HoldBound::Within(crate::app_update_handoff_boundary::HANDOFF_PENDING_BOUND)
            }
            Self::ConsentWarmup => HoldBound::Within(Duration::from_millis(
                crate::app_config::PRIVACY_WARMUP_HOLD_MS_MAX,
            )),
            Self::RestoredAgents => HoldBound::Within(crate::harness_host::RESTORED_HOLD),
            Self::CaptureRefusal => HoldBound::Within(crate::app_native::CAPTURE_REFUSAL_BACKSTOP),
            Self::Activity | Self::Recording => {
                HoldBound::Within(crate::native_update_auto_intent::LANDS_WITHIN)
            }
            Self::PasswordPrompt => HoldBound::PersonNow(PersonHold::PasswordPrompt),
            Self::DeveloperTools => HoldBound::PersonNow(PersonHold::DeveloperTools),
            Self::SetupDraft => HoldBound::PersonNow(PersonHold::SetupDraft),
            Self::UpdaterBusy
            | Self::PersonInstall
            | Self::StagePending
            | Self::ActivationSupersede
            | Self::RosterConfirmation
            | Self::PreparkRead
            | Self::LaunchRecordRenewal
            | Self::BoundaryItem
            | Self::SetupAnswer
            | Self::SystemState
            | Self::TabStarting
            | Self::PaneExited
            | Self::PolicyRead
            | Self::ActivityRevoked => HoldBound::Program,
        }
    }

    /// The bound as the line says it after [`Self::lifts`]: the time it lifts within, or that it
    /// holds only while the person's own dialog or draft is there; nothing for a hold this program
    /// lifts itself.
    #[must_use]
    pub(crate) fn bound_words(self) -> String {
        match self.bound() {
            HoldBound::Within(bound) => format!(" (at most {} s)", bound.as_secs()),
            HoldBound::Program => String::new(),
            HoldBound::PersonNow(person) => person.words().to_string(),
        }
    }

    /// Whether another line already names this hold when it begins (the restored agents'
    /// `native_update_auto_intent::restored_hold_line`, the roster wait's, each activity
    /// revocation's with its retry), so [`NamedHold`] keeps it for `update status` without a
    /// second line.
    #[must_use]
    pub(crate) const fn said_elsewhere(self) -> bool {
        matches!(
            self,
            Self::RestoredAgents | Self::RosterConfirmation | Self::ActivityRevoked
        )
    }
}

/// A hold as the lane named it: which, and since when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeldLane {
    pub(crate) hold: LaneHold,
    /// The poll that first found it.
    pub(crate) since: Instant,
}

/// The hold an armed intent is under past its ladder, as it was named (`AutoApplyIntent::held`):
/// what makes the log say a hold once per change, never once per poll. It lives in the intent, so
/// it never outlives it: a dropped intent takes it along, and a new arming starts with none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct NamedHold(Option<HeldLane>);

impl NamedHold {
    /// A poll in `Land` declined build `build` under `hold`, whose refusal said `reason`. The
    /// line to say when this hold is new (`None` when it was already named, or when
    /// [`LaneHold::said_elsewhere`]).
    pub(crate) fn hold(
        &mut self,
        build: u64,
        hold: LaneHold,
        reason: &str,
        now: Instant,
        now_unix: u64,
    ) -> Option<String> {
        if self.0.is_some_and(|held| held.hold == hold) {
            return None;
        }
        self.0 = Some(HeldLane { hold, since: now });
        (!hold.said_elsewhere()).then(|| held_line(build, hold, reason, now_unix))
    }

    /// The lane attempted build `build`, or dropped its intent: the line that says the hold is
    /// over, when one was named.
    pub(crate) fn release(&mut self, build: u64, now: Instant) -> Option<String> {
        let held = self.0.take()?;
        (!held.hold.said_elsewhere())
            .then(|| released_line(build, held.hold, now.saturating_duration_since(held.since)))
    }

    /// The hold named now, if any.
    #[must_use]
    pub(crate) const fn current(self) -> Option<HeldLane> {
        self.0
    }
}

/// Where the lane's hold lines go: the log, or, in a test App, a list the test reads.
#[derive(Debug, Default)]
pub(crate) enum HoldWords {
    /// `aterm_log::info!`, the owner's log.
    #[default]
    Log,
    /// Kept for a test to read.
    #[cfg(any(test, feature = "bench-support"))]
    Kept(Vec<String>),
}

impl HoldWords {
    /// Say `line`.
    pub(crate) fn say(&mut self, line: String) {
        match self {
            Self::Log => aterm_log::info!("{line}"),
            #[cfg(any(test, feature = "bench-support"))]
            Self::Kept(lines) => lines.push(line),
        }
    }

    /// The lines said so far, in a test App (the lane's App tests run on macOS).
    #[cfg(all(test, target_os = "macos"))]
    #[must_use]
    pub(crate) fn kept(&self) -> &[String] {
        match self {
            Self::Log => &[],
            Self::Kept(lines) => lines.as_slice(),
        }
    }
}

/// The line that names a hold as it begins.
#[must_use]
pub(crate) fn held_line(build: u64, hold: LaneHold, reason: &str, since_unix: u64) -> String {
    format!(
        "update auto-apply for build {build}: held by {reason} ({}) since {}; it lands when {}{}",
        hold.token(),
        aterm_types::rfc3339::format_rfc3339(since_unix),
        hold.lifts(),
        hold.bound_words()
    )
}

/// The line that says a named hold is over, `lasted` after it began.
#[must_use]
pub(crate) fn released_line(build: u64, hold: LaneHold, lasted: Duration) -> String {
    format!(
        "update auto-apply for build {build}: the hold by {} is over after {} s",
        hold.token(),
        lasted.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every hold has a token of its own and a bound none of which is a person's event; a hold
    /// that holds on a person is that person's own state now, by its own [`PersonHold`].
    #[test]
    fn every_hold_is_bounded_by_time_the_program_or_a_persons_state_now() {
        let mut tokens = std::collections::BTreeSet::new();
        for &hold in LaneHold::ALL {
            assert!(tokens.insert(hold.token()), "{hold:?}: a token of its own");
            assert!(!hold.lifts().is_empty(), "{hold:?}");
            match hold.bound() {
                HoldBound::Within(bound) => assert!(
                    bound > Duration::ZERO && bound <= Duration::from_secs(60 * 60),
                    "{hold:?}: {bound:?}"
                ),
                HoldBound::Program => {}
                HoldBound::PersonNow(person) => assert_eq!(
                    person.hold(),
                    hold,
                    "{hold:?} holds on a person's state that is not its own"
                ),
            }
        }
    }

    /// A hold is said once when it begins, never per poll, again when it changes, and its end
    /// once; each line carries the build, the hold's token, its start, what lifts it and its bound.
    #[test]
    fn a_hold_is_said_once_per_change_and_once_when_it_ends() {
        let start = Instant::now();
        let at = |s: u64| start + Duration::from_secs(s);
        let since_unix = 1_793_338_376;
        let mut named = NamedHold::default();
        let line = named
            .hold(7, LaneHold::SetupDraft, "a draft", at(0), since_unix)
            .expect("a new hold is said");
        for part in [
            "build 7",
            "a draft",
            "(setup-draft)",
            &aterm_types::rfc3339::format_rfc3339(since_unix),
            LaneHold::SetupDraft.lifts(),
            &LaneHold::SetupDraft.bound_words(),
        ] {
            assert!(line.contains(part), "{line:?} says {part:?}");
        }
        for s in 1..=600 {
            assert_eq!(
                named.hold(7, LaneHold::SetupDraft, "other words", at(s), 1),
                None,
                "poll {s}: the same hold is not said again"
            );
        }
        assert_eq!(named.current().map(|held| held.since), Some(at(0)));
        assert!(
            named
                .hold(7, LaneHold::SystemState, "an Apply", at(601), 2)
                .is_some_and(|line| line.contains("(system-state)")),
            "a change of hold is said"
        );
        let end = named.release(7, at(700)).expect("the end is said");
        assert_eq!(
            end,
            released_line(7, LaneHold::SystemState, Duration::from_secs(99))
        );
        assert_eq!(named.release(7, at(701)), None, "an end is said once");
        assert!(named.current().is_none());
    }

    /// A hold another line already names is kept for the status and says nothing of itself.
    #[test]
    fn a_hold_said_elsewhere_is_kept_but_not_said_twice() {
        let now = Instant::now();
        for hold in [
            LaneHold::RestoredAgents,
            LaneHold::RosterConfirmation,
            LaneHold::ActivityRevoked,
        ] {
            let mut named = NamedHold::default();
            assert_eq!(named.hold(7, hold, "words", now, 1), None, "{hold:?}");
            assert_eq!(named.current().map(|held| held.hold), Some(hold));
            assert_eq!(named.release(7, now), None, "{hold:?}");
            assert_eq!(named.current(), None);
        }
    }
}
