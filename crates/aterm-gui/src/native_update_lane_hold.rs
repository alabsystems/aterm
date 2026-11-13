// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What holds the automatic update lane once its ladder says `Land`, and how each hold is named.
//!
//! Past [`crate::native_update_auto_intent::LANDS_WITHIN`] the ladder no longer waits for the
//! terminal, so a poll that still does not attempt is held by something else: a hold. Every hold
//! the lane honours is a [`LaneHold`], and every one states its bound ([`LaneHold::bound`]): it
//! lifts within a fixed time, at an event this program makes by itself, or, for a person's own
//! state (a tab waiting on them, a draft or choice they made), only while that state is true. None
//! waits for a window to take an event: a hold that did (the System State offer of 0.146.0,
//! answered only once a person was at the screen) kept an owner's staged update waiting 3.45 days
//! for a touch.
//!
//! The lane names the hold it is under where a person and a program can read it ([`NamedHold`],
//! kept in the armed intent so it never outlives it): one info line when a hold begins or
//! changes (never one per poll), one when it ends, and `apply_held=<token>` in
//! `aterm ctl update status`.

use std::time::{Duration, Instant};

/// The `password-prompt` hold's bound: the askpass relay withdraws every password sheet by its
/// deadline (`crate::askpass::ASKPASS_DEADLINE`).
pub(crate) const PASSWORD_PROMPT_BOUND: Duration = Duration::from_secs(300);

#[cfg(any(target_os = "macos", test))]
const _: () =
    assert!(PASSWORD_PROMPT_BOUND.as_secs() == crate::askpass::ASKPASS_DEADLINE.as_secs());

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
    /// A person's Install is held for the candidate's pre-park read, which ends by itself, or
    /// for the startup boot-health gate, which ends on its own clock (`BootHealth`).
    PersonInstall => "person-install",
    /// This build's own update trial is unconfirmed, or its confirmation has not answered yet.
    /// The updater answers each ask: it confirms the trial, or settles it after
    /// `aterm_update::TRIAL_SETTLE_AFTER_REFUSALS` refused proofs spanning
    /// `aterm_update::TRIAL_SETTLE_REFUSAL_SPAN` of awake time; `crate::BOOT_HEALTH_UNANSWERED_TRIES`
    /// asks nothing answers open the gate, and so does `crate::BOOT_HEALTH_PENDING_PATIENCE`
    /// of the updater's Pending answers (`aterm_update::BootHealthVerdict`), said in the hold's
    /// words. Past the gate the apply lane's own admission decides with the trial armed, and its
    /// refusal for this build's own trial names this hold too, read again every
    /// `crate::app_native::BOOT_HEALTH_TRIAL_REREAD`.
    BootHealth => "boot-health",
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
    /// bootstrap, a harness's native mutation) is in its critical section, which is aterm's own
    /// work: each try follows it for at most
    /// [`crate::app_update_handoff_boundary::MAX_BOUNDARY_WAIT`], and the next try follows it
    /// again, so the hold lasts as long as the item's section does. A harness native mutation's
    /// section ends once it can start nothing more, though its processes run on
    /// (`native_act::starts_nothing_more`): a signalled restart past its deadline, an own-image
    /// move whose actor is gone. A restart that granted its child Execute, and an own-image move
    /// whose actor lives on, hold until they finish or their processes exit.
    BoundaryItem => "boundary-item",
    /// A launch's Mac setup offer has a person's draft in it (a group pressed, a vault address or
    /// password typed), which the successor could not carry. It is the person's own and holds only
    /// while it is there; an offer nobody touched holds nothing, whatever the setup worker is doing
    /// (`SystemStateApp::offer_hold`).
    SetupDraft => "setup-draft",
    /// A launch's Mac setup offer is being answered by the page itself: its own answer, a queued
    /// page action or a Stop is under way, which the page settles by itself at its next ticks;
    /// at most [`crate::workstation_setup::SETUP_ANSWER_BACKSTOP`], past which the offer holds
    /// only for a person's draft in it.
    SetupAnswer => "setup-answer",
    /// A System State Apply nobody chose is still finding its footing; it holds at most
    /// [`crate::system_state_host::UNPRESSED_APPLY_SETTLE`].
    SystemState => "system-state",
    /// A setup choice someone made (an Apply of a chosen plan, a Restore, a choice queued behind
    /// another setup) is not yet durable, so a handoff would drop it: it is that person's or
    /// client's unsaved work and holds only while the setup worker has not taken it (a Restore:
    /// finished it).
    SetupChoice => "setup-choice",
    /// A Mac Setup tab (a Command Line Tools install's, or a naming permission's repair) is still
    /// running and cannot travel with the handoff (its password prompt waiting on the person, or
    /// a child the successor could not recover): it holds only while that tab's program runs, and
    /// an update's apply stops a Command Line Tools install whose password prompt nobody touched
    /// for `crate::clt_install::UNTOUCHED_PROMPT_BACKSTOP`.
    DeveloperTools => "developer-tools",
    /// aterm's own sudo password sheet is up: every sheet is withdrawn within
    /// [`PASSWORD_PROMPT_BOUND`], and one nobody has touched for the askpass relay's untouched
    /// backstop (`crate::askpass::ASKPASS_UNTOUCHED_BACKSTOP`) is refused at the next attempt.
    PasswordPrompt => "password-prompt",
    /// A new tab is still starting; a spawn is bounded by `aterm_pty::START_HANG`.
    TabStarting => "tab-starting",
    /// Input or PTY output inside the quiet epoch, before the reader park; only an `Automatic`
    /// (before-grace) attempt reads it, so it never holds past `Land`.
    Activity => "activity",
    /// A handed session's command exited while its pane is still open; its reader sees the
    /// hang-up and retires it, and a session hung up for
    /// [`crate::app_update_handoff::PANE_EXITED_BACKSTOP`] is booked exited without it.
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
/// ([`HoldBound::PersonNow`]): a Command Line Tools tab waiting on them, a draft they typed and
/// have not applied, or a choice not yet saved. Each is one [`LaneHold`] ([`Self::hold`]), and
/// holds only while it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonHold {
    /// A Mac Setup tab that cannot travel with the handoff, its password prompt waiting on them
    /// while they are at it: a Command Line Tools install's one nobody touched for
    /// `crate::clt_install::UNTOUCHED_PROMPT_BACKSTOP` is stopped by an update's apply.
    DeveloperTools,
    /// Their draft in the launch's setup offer.
    SetupDraft,
    /// A setup choice they (or a client) made that the setup worker has not taken yet, or a
    /// Restore they started that it has not finished.
    SetupChoice,
}

impl PersonHold {
    /// The lane hold this person's state is.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn hold(self) -> LaneHold {
        match self {
            Self::DeveloperTools => LaneHold::DeveloperTools,
            Self::SetupDraft => LaneHold::SetupDraft,
            Self::SetupChoice => LaneHold::SetupChoice,
        }
    }

    /// The bound as the line says it: whose state it is, and that it holds only while it is.
    #[must_use]
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::DeveloperTools => {
                " (a person's own setup tab: it holds only while it runs, and a Command Line \
                 Tools install's password prompt only while someone answers it)"
            }
            Self::SetupDraft => " (a person's own draft: it holds only while it is there)",
            Self::SetupChoice => {
                " (a choice someone made and not yet saved: it holds only while it is unsaved)"
            }
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
            Self::PersonInstall => {
                "the candidate's pre-park read ends, or the startup boot-health gate opens"
            }
            Self::BootHealth => {
                "this build's boot-health confirmation lands, its bound settles the trial, or the \
                 startup gate's patience passes and the apply admission decides"
            }
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
            Self::SystemState => "the Apply nobody chose is taken by the setup worker",
            Self::SetupChoice => {
                "the setup worker takes the chosen Apply or queued choice or finishes the \
                 Restore, or it is stopped"
            }
            Self::DeveloperTools => {
                "the setup tab ends, or a Command Line Tools install's untouched password prompt \
                 is stopped"
            }
            Self::PasswordPrompt => {
                "the sudo password prompt is answered, closed or withdrawn, or refused untouched"
            }
            Self::TabStarting => "the new tab has started",
            Self::Activity => "a quiet epoch passes",
            Self::PaneExited => "the exited pane's reader retires it, or aterm books it exited",
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
            Self::PasswordPrompt => HoldBound::Within(PASSWORD_PROMPT_BOUND),
            Self::DeveloperTools => HoldBound::PersonNow(PersonHold::DeveloperTools),
            Self::SetupDraft => HoldBound::PersonNow(PersonHold::SetupDraft),
            Self::SetupChoice => HoldBound::PersonNow(PersonHold::SetupChoice),
            Self::SetupAnswer => HoldBound::Within(crate::workstation_setup::SETUP_ANSWER_BACKSTOP),
            Self::SystemState => {
                HoldBound::Within(crate::system_state_host::UNPRESSED_APPLY_SETTLE)
            }
            Self::PaneExited => HoldBound::Within(crate::app_update_handoff::PANE_EXITED_BACKSTOP),
            Self::UpdaterBusy
            | Self::PersonInstall
            | Self::BootHealth
            | Self::StagePending
            | Self::ActivationSupersede
            | Self::RosterConfirmation
            | Self::PreparkRead
            | Self::LaunchRecordRenewal
            | Self::BoundaryItem
            | Self::TabStarting
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

/// How long one condition has stood at every automatic attempt in a row: the clock a hold's own
/// backstop reads. It is fed the condition's raw state, never what a backstop made of it, so the
/// bound it reaches stays reached while the condition does. Two looks further apart than
/// [`Self::SAMPLE_GAP`] are two episodes, so a clock left standing while nothing attempted never
/// counts the time between them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HoldClock {
    since: Option<Instant>,
    seen: Option<Instant>,
}

impl HoldClock {
    /// The longest gap between two looks of one episode: well past the lane's slowest re-probe
    /// (`PREFLIGHT_BLOCK_COOLDOWN`, 60 s).
    pub(crate) const SAMPLE_GAP: Duration = Duration::from_secs(5 * 60);

    /// One attempt's look: the condition `holds` now, or not.
    pub(crate) fn observe(&mut self, holds: bool, now: Instant) {
        if !holds {
            *self = Self::default();
            return;
        }
        let continuous = self
            .seen
            .is_some_and(|seen| now.saturating_duration_since(seen) <= Self::SAMPLE_GAP);
        self.since = Some(match self.since {
            Some(since) if continuous => since,
            _ => now,
        });
        self.seen = Some(now);
    }

    /// Whether the condition has stood for `bound` by `now`. A clock whose last look is more than
    /// [`Self::SAMPLE_GAP`] before `now` says nothing: the condition it saw may have cleared and
    /// come back unseen, so a reader between attempts never takes an old episode for a new one.
    #[must_use]
    pub(crate) fn overdue(&self, bound: Duration, now: Instant) -> bool {
        let looked = self
            .seen
            .is_some_and(|seen| now.saturating_duration_since(seen) <= Self::SAMPLE_GAP);
        looked
            && self
                .since
                .is_some_and(|since| now.saturating_duration_since(since) >= bound)
    }

    /// A clock that has seen its condition since `ago` before now, at every attempt.
    #[cfg(all(test, unix))]
    pub(crate) fn backdated_for_test(ago: Duration) -> Self {
        let now = Instant::now();
        Self {
            since: now.checked_sub(ago),
            seen: Some(now),
        }
    }
}

const _: () = assert!(HoldClock::SAMPLE_GAP.as_secs() >= 4 * 60);

/// The clocks of the holds whose backstop the lane keeps itself (`App::tick_lane_hold_clocks`).
#[derive(Debug, Default)]
pub(crate) struct LaneHoldClocks {
    /// aterm's own document work in the close preflight's way (`App::own_document_work_overdue`).
    pub(crate) own_documents: HoldClock,
    /// The setup page's own answer to a launch's offer ([`LaneHold::SetupAnswer`]).
    #[cfg(unix)]
    pub(crate) setup_answer: HoldClock,
    /// Each handed session whose master has hung up, from the first attempt that saw it so
    /// ([`LaneHold::PaneExited`]); a hang-up does not un-hang, so it needs no episode.
    #[cfg(unix)]
    pub(crate) hung_up: std::collections::BTreeMap<u64, Instant>,
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
#[path = "native_update_lane_hold_tests.rs"]
mod tests;
