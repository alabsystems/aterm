// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renewing a stale LaunchServices record, an inherent-impl split of `App`.
//!
//! A process LaunchServices launched that then `execve`s in place keeps a stale record
//! for life: LaunchServices lists the app with no pid (`-1`), so System Settings' Quit &
//! Reopen misses it, its menu-bar item is never placed, and TCC attributes it to the
//! bundle it first saw ([`crate::app_launch_successor::OwnLaunchRecord::Stale`]). A
//! running process recovers only through a fresh launch, and a fixed build cannot stop
//! every way a process ends up stale (an older build's successor that swapped and
//! re-exec'd itself, a window's left-stage escape, causes not yet measured). So a stale
//! process renews its own record: a same-image seamless handoff
//! ([`crate::app_update_handoff::SameImageHandoff::RenewLaunchRecord`]) to a fresh
//! LaunchServices launch of this very build, whose record is sound (measured
//! 2026-10-20: a stale parent's `openApplicationAtURL` successor is named with its pid).
//!
//! * WHEN ([`decide`]): only with `[update] auto_apply` on (nothing replaces the process
//!   unasked otherwise, so switching it off ends the renewal, an attempt in flight
//!   included), the overlap handoff available, a clean commit stamp, no update staged or
//!   in flight (the update lane's own handoff renews the record as it lands), the
//!   installed bundle this build, and a live terminal session to hand over. A renewal's
//!   successor never renews again.
//! * HOW: the automatic update lane's ladder and gates, read-only
//!   ([`crate::native_update_auto_intent::apply_phase`],
//!   [`crate::native_update_auto_intent::automatic_park_refusal`], the launched lane's
//!   park gate), with the renewal's own anchor, capped below `Land` so it never parks over
//!   typing. The LaunchServices (out-of-band) lane only: the fork lane's successor keeps
//!   this process's launchd job, which ends with it, and the cold lane execs in place,
//!   which is what leaves the record stale.
//! * BOUND: [`ATTEMPTS`] counted attempts per process, spaced by [`SPACING`]; an activity
//!   deferral or a held close preflight costs none. Exhausted or ruled out, the digest's
//!   entry gives the person the step.

use std::time::{Duration, Instant};

use crate::app_launch_successor::OwnLaunchRecord;
use crate::native_update_auto_intent::ApplyPhase;
use crate::native_updater_service::ApplyMode;

/// The lineage marker a renewal's successor is launched with: the parent's pid. Taken at
/// startup (`main_entry`), so no shell inherits it. Lineage, not authority: a value only
/// stops a renewal, and none grants one. Not an `ATERM_HANDOFF_*` name, which the census
/// admits only as a handoff authority name.
pub(crate) const RENEWED_FROM_ENV: &str = "ATERM_LAUNCH_RECORD_RENEWED_FROM";

/// Counted renewal attempts per process. Every one that launched a successor costs a
/// launch, so this bounds the launches a terminal that never pauses can cause.
pub(crate) const ATTEMPTS: u8 = 3;

/// The wait before the attempt after the `k`-th counted one (`SPACING[k - 1]`).
const SPACING: [Duration; ATTEMPTS as usize - 1] =
    [Duration::from_secs(30), Duration::from_secs(300)];

/// How soon an armed renewal that waits for update work looks again: the work ends by
/// itself, soon.
const WORK_REPOLL: Duration = Duration::from_secs(2);

/// How soon a renewal the person's live state holds looks again: unsaved native work, or
/// no live terminal session to hand over (every pane's command exited under `--hold`, or
/// only documents and Settings open). Each changes only when the person acts, so a short
/// repoll would wake an idle aterm for nothing; the automatic lane re-checks the same
/// terminal wait on its 60 s block cooldown. Never a failed attempt.
const PERSON_STATE_REPOLL: Duration = Duration::from_secs(60);

/// The facts [`decide`] reads, every one a memory read on the main thread: no filesystem
/// probe and no codesign (the worker proves the installed bundle before it launches).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RenewalFacts {
    /// This process is a renewal's successor, launched from that pid ([`RENEWED_FROM_ENV`]).
    pub(crate) renewed_from: Option<u32>,
    /// `[update] auto_apply` is on, and no QA seam suppresses the automatic lane.
    pub(crate) auto_apply: bool,
    /// `App::seamless_handoff_unavailable` names a reason.
    pub(crate) handoff_unavailable: bool,
    /// This build's commit stamp is clean (`aterm_update::commit_matches` with itself):
    /// the adoption proof hashes it, and the worker matches the installed bundle by it.
    pub(crate) clean_commit: bool,
    /// A stage is on record, or the automatic lane holds an intent.
    pub(crate) update_pending: bool,
    /// The installed bundle's build as the last updater observation read it; `None`
    /// when not yet observed.
    pub(crate) installed_build: Option<u64>,
    /// This process's build.
    pub(crate) current_build: u64,
    /// A check, download or apply is in flight, a handoff is, or a person's Install is
    /// held.
    pub(crate) update_work: bool,
    /// Counted attempts so far.
    pub(crate) attempts: u8,
    /// A terminal session with a live process is open (`App::handoff_live_sessions`).
    pub(crate) live_terminal: bool,
}

/// What keeps an armed renewal waiting, booking nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenewalWait {
    /// Update work in flight may stage a build, whose own handoff renews the record.
    Work,
    /// No live terminal session, so no overlap lane to hand over on.
    Terminal,
}

impl RenewalWait {
    /// How soon a renewal this keeps waiting looks again.
    fn repoll(self) -> Duration {
        match self {
            Self::Work => WORK_REPOLL,
            Self::Terminal => PERSON_STATE_REPOLL,
        }
    }
}

/// Why no renewal runs in this process at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoRenewal {
    UpdatesOff,
    HandoffUnavailable,
    Unstamped,
    InstalledIsAnotherBuild,
}

impl NoRenewal {
    /// The clause the log line ends with.
    #[must_use]
    pub(crate) fn cause(self) -> &'static str {
        match self {
            Self::UpdatesOff => "[update] auto_apply is off, so nothing replaces it unasked",
            Self::HandoffUnavailable => "the overlap handoff is unavailable in this process",
            Self::Unstamped => "this build has no clean commit stamp for a handoff to prove",
            Self::InstalledIsAnotherBuild => {
                "the installed bundle is another build, and a renewal launches only this one"
            }
        }
    }
}

/// Which digest entry a renewal that ends without renewing books.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BookCause {
    /// This process is a renewal's successor, and its record is stale too.
    StillStale { from: u32 },
    /// No renewal runs here ([`NoRenewal`]).
    NoRenewal(NoRenewal),
    /// Every counted attempt was spent.
    Spent { attempts: u8 },
}

/// What a stale record's renewal does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenewalDecision {
    /// Try it now, at the gates' answer.
    Renew,
    /// Keep it armed and book nothing.
    Wait(RenewalWait),
    /// A stage is on record: the update lane's own handoff renews the record as it lands.
    LeaveToUpdate,
    /// End it and book the entry.
    Book(BookCause),
}

/// THE ARMING DECISION: the first rule that holds, in this order.
///
/// A marked successor never renews. With the automatic lane off, no overlap handoff, or
/// no clean commit stamp (which the adoption proof and the worker's bundle check both
/// refuse), no renewal can run. A pending update belongs to the update lane, ahead of the
/// installed-bundle rule (an installed newer build is imported as a stage). Update work in
/// flight may stage one, so it is waited for, ahead of exhaustion, so a stage that
/// arrives with the last failure is left to the update. With no live terminal there is
/// no overlap lane: wait for one.
#[must_use]
pub(crate) fn decide(facts: RenewalFacts) -> RenewalDecision {
    if let Some(from) = facts.renewed_from {
        return RenewalDecision::Book(BookCause::StillStale { from });
    }
    let no_renewal = if !facts.auto_apply {
        Some(NoRenewal::UpdatesOff)
    } else if facts.handoff_unavailable {
        Some(NoRenewal::HandoffUnavailable)
    } else if !facts.clean_commit {
        Some(NoRenewal::Unstamped)
    } else {
        None
    };
    if let Some(why) = no_renewal {
        return RenewalDecision::Book(BookCause::NoRenewal(why));
    }
    if facts.update_pending {
        return RenewalDecision::LeaveToUpdate;
    }
    if facts
        .installed_build
        .is_some_and(|installed| installed != facts.current_build)
    {
        return RenewalDecision::Book(BookCause::NoRenewal(NoRenewal::InstalledIsAnotherBuild));
    }
    if facts.update_work {
        return RenewalDecision::Wait(RenewalWait::Work);
    }
    if facts.attempts >= ATTEMPTS {
        return RenewalDecision::Book(BookCause::Spent {
            attempts: facts.attempts,
        });
    }
    if !facts.live_terminal {
        return RenewalDecision::Wait(RenewalWait::Terminal);
    }
    RenewalDecision::Renew
}

/// The renewal's ladder phase, `since_armed` after its anchor: the automatic lane's, capped
/// at `KeysOnly`, so it never parks over typing (`Land` would).
fn renewal_phase(since_armed: Duration) -> ApplyPhase {
    crate::native_update_auto_intent::apply_phase(since_armed).min(ApplyPhase::KeysOnly)
}

/// The mode an attempt in `phase` runs under, as the automatic lane maps it: `Automatic`
/// waits for a quiet moment at its entry and park, `AutomaticPastGrace` reads the ladder.
fn renewal_mode(phase: ApplyPhase) -> ApplyMode {
    if phase == ApplyPhase::PreferIdle {
        ApplyMode::Automatic
    } else {
        ApplyMode::AutomaticPastGrace
    }
}

/// This process's renewal of its stale LaunchServices record (`App::launch_record_renewal`).
pub(crate) struct LaunchRecordRenewal {
    /// The parent's pid when this process is a renewal's successor ([`RENEWED_FROM_ENV`]).
    pub(crate) renewed_from: Option<u32>,
    attempts: u8,
    state: RenewalState,
}

enum RenewalState {
    /// Nothing measured stale, or not yet.
    Idle,
    /// Looks again at `retry_at`; the ladder is anchored at `armed_at`.
    Armed {
        armed_at: Instant,
        retry_at: Instant,
    },
    /// An attempt's successor is launching or held. `ending`: why its return ends the
    /// renewal instead of re-arming it, once something has.
    InFlight {
        armed_at: Instant,
        ending: Option<InFlightEnd>,
    },
    /// Settled: renewed by nothing in this process, and booked as such.
    Ended,
}

/// Why an attempt in flight ends the renewal at its return instead of re-arming it. The
/// first reason stands, as [`decide`] would rank them.
#[derive(Clone, Copy)]
enum InFlightEnd {
    /// `[update] auto_apply` went off: nothing replaces this process unasked.
    UpdatesOff,
    /// An update was staged: its own handoff renews the record as it lands.
    LeaveToUpdate,
}

impl InFlightEnd {
    /// What the renewal settles at the attempt's return.
    fn settles(self) -> RenewalDecision {
        match self {
            Self::UpdatesOff => RenewalDecision::Book(BookCause::NoRenewal(NoRenewal::UpdatesOff)),
            Self::LeaveToUpdate => RenewalDecision::LeaveToUpdate,
        }
    }

    /// The stand-down's account, for the worker's completion and aterm.log.
    fn stand_down_detail(self) -> &'static str {
        match self {
            Self::UpdatesOff => {
                "[update] auto_apply was switched off; a launch-record renewal replaces nothing \
                 unasked"
            }
            Self::LeaveToUpdate => "an update was staged; its own handoff renews the launch record",
        }
    }
}

impl LaunchRecordRenewal {
    pub(crate) fn new(renewed_from: Option<u32>) -> Self {
        Self {
            renewed_from,
            attempts: 0,
            state: RenewalState::Idle,
        }
    }

    /// Whether an armed renewal's next look has come.
    pub(crate) fn due(&self, now: Instant) -> bool {
        matches!(self.state, RenewalState::Armed { retry_at, .. } if now >= retry_at)
    }

    /// An armed renewal's next look, for the event loop's deadline.
    pub(crate) fn wake_at(&self) -> Option<Instant> {
        match self.state {
            RenewalState::Armed { retry_at, .. } => Some(retry_at),
            _ => None,
        }
    }

    /// The phase the launched lane's park gate reads for a renewal's attempt at `now`
    /// ([`renewal_phase`]); the strictest, `PreferIdle`, with no anchor.
    pub(crate) fn gate_phase(&self, now: Instant) -> ApplyPhase {
        match self.state {
            RenewalState::Armed { armed_at, .. } | RenewalState::InFlight { armed_at, .. } => {
                renewal_phase(now.saturating_duration_since(armed_at))
            }
            RenewalState::Idle | RenewalState::Ended => ApplyPhase::PreferIdle,
        }
    }

    /// Re-arms after a counted attempt: the next one after its spacing, with the ladder
    /// anchored afresh there, so it prefers an idle moment first; once every attempt is
    /// spent, at once, so the next look books.
    fn rearm_after_attempt(&mut self, now: Instant) {
        let retry_at = SPACING
            .get(usize::from(self.attempts.saturating_sub(1)))
            .map_or(now, |gap| now + *gap);
        self.state = RenewalState::Armed {
            armed_at: retry_at,
            retry_at,
        };
    }
}

/// What the quiet close preflight answers a renewal
/// (`App::quiet_native_update_safety_token`).
pub(crate) enum QuietPreflight {
    /// Ready, with the evidence the attempt carries.
    Ready(crate::app_native::NativeUpdateSafetyToken),
    /// Unsaved native work holds it: the person's live state, not a failed attempt.
    Held(Vec<String>),
    /// The close reducer broke, which waiting never repairs.
    Broken(String),
}

impl crate::App {
    /// The facts [`decide`] reads, as they stand now.
    fn renewal_facts(&self) -> RenewalFacts {
        let updater = self.native_updater_service.snapshot();
        let commit = crate::build_info::git_commit();
        RenewalFacts {
            renewed_from: self.launch_record_renewal.renewed_from,
            auto_apply: crate::app_config::update_auto_apply(&self.config)
                && !Self::relaunch_nudge_seam_suppresses_auto_apply(),
            handoff_unavailable: self.seamless_handoff_unavailable().is_some(),
            clean_commit: aterm_update::commit_matches(commit, commit),
            update_pending: updater.staged.is_some() || self.auto_apply_intent.is_some(),
            installed_build: self.native_installed_trial.map(|(build, _)| build),
            current_build: crate::running_build_number(),
            update_work: updater.active.is_some()
                || updater.phase == crate::native_updater_service::UpdaterPhase::Applying
                || self.update_handoff_in_flight()
                || self.person_apply_is_held(),
            attempts: self.launch_record_renewal.attempts,
            live_terminal: !self.handoff_live_sessions().is_empty(),
        }
    }

    /// Whether `SameImageHandoff::RenewLaunchRecord` is granted now: this process measured
    /// its own record stale, is no renewal's successor, and its renewal is armed (the
    /// poll's call, before it moves to `InFlight`). Naming the variant grants nothing.
    pub(crate) fn launch_record_renewal_authorizes(&self) -> bool {
        self.own_launch_record == Some(OwnLaunchRecord::Stale)
            && self.launch_record_renewal.renewed_from.is_none()
            && matches!(self.launch_record_renewal.state, RenewalState::Armed { .. })
    }

    /// `OwnLaunchRecord::Stale`, as measured (`App::note_own_launch_record`): arms the
    /// renewal, or settles it at once when nothing will renew the record here. While it
    /// is armed no entry stands: this process books none, and withdraws the one it
    /// adopted from its parent at Commit (`App::book_launch_unregistered`, the same key),
    /// so the person is told to quit and open aterm again only once no renewal runs
    /// ([`Self::settle_launch_record_renewal`] books it then).
    pub(crate) fn note_stale_launch_record(&mut self, now: Instant) {
        if !matches!(self.launch_record_renewal.state, RenewalState::Idle) {
            return;
        }
        let pid = std::process::id();
        match decide(self.renewal_facts()) {
            RenewalDecision::Renew | RenewalDecision::Wait(_) => {
                aterm_log::warn!(
                    "launch record: LaunchServices lists this app with no pid and never as pid \
                     {pid}; a handoff to a fresh launch of build {} renews it at a quiet moment",
                    crate::running_build_number()
                );
                self.except_end(&crate::update_words::launch_unregistered_entry(0).key());
                self.launch_record_renewal.state = RenewalState::Armed {
                    armed_at: now,
                    retry_at: now,
                };
            }
            settled => self.settle_launch_record_renewal(settled),
        }
    }

    /// One look of an armed renewal whose time has come (`about_to_wait`): the decision
    /// again, then the automatic lane's entry gate at the renewal's phase, the quiet close
    /// preflight, and the attempt.
    pub(crate) fn poll_launch_record_renewal(&mut self, now: Instant) {
        let RenewalState::Armed { armed_at, .. } = self.launch_record_renewal.state else {
            return;
        };
        let wait_until = |retry_at| RenewalState::Armed { armed_at, retry_at };
        match decide(self.renewal_facts()) {
            RenewalDecision::Renew => {}
            RenewalDecision::Wait(wait) => {
                self.launch_record_renewal.state = wait_until(now + wait.repoll());
                return;
            }
            settled => {
                self.settle_launch_record_renewal(settled);
                return;
            }
        }
        // The entry gate, as the automatic lane's: a refusal costs nothing.
        let phase = renewal_phase(now.saturating_duration_since(armed_at));
        let mode = renewal_mode(phase);
        if let Some(reason) = crate::native_update_auto_intent::automatic_park_refusal(
            phase,
            self.automatic_activity_facts(mode, now),
        ) {
            aterm_log::debug!("launch record: the renewal waits in {phase:?}: {reason}");
            self.launch_record_renewal.state =
                wait_until(crate::automatic_update_activity_retry_at(now));
            return;
        }
        let k = self.launch_record_renewal.attempts.saturating_add(1);
        let token = match self.quiet_native_update_safety_token() {
            QuietPreflight::Ready(token) => token,
            QuietPreflight::Held(reasons) => {
                aterm_log::debug!(
                    "launch record: the renewal waits for unsaved work: {}",
                    reasons.join("; ")
                );
                self.launch_record_renewal.state = wait_until(now + PERSON_STATE_REPOLL);
                return;
            }
            QuietPreflight::Broken(why) => {
                aterm_log::warn!(
                    "launch record: attempt {k} of {ATTEMPTS} did not start: the close \
                     preflight failed ({why})"
                );
                self.launch_record_renewal.attempts = k;
                self.launch_record_renewal.rearm_after_attempt(now);
                return;
            }
        };
        aterm_log::info!(
            "launch record: renewing this process's LaunchServices record (pid {}) by a \
             handoff to a fresh launch of build {} (attempt {k} of {ATTEMPTS})",
            std::process::id(),
            crate::running_build_number()
        );
        // Still `Armed` during the call, which is what grants the variant.
        match self.apply_staged_update_now(
            token,
            mode,
            None,
            Some(crate::app_update_handoff::SameImageHandoff::RenewLaunchRecord),
        ) {
            Ok(()) => {
                self.launch_record_renewal.attempts = k;
                self.launch_record_renewal.state = RenewalState::InFlight {
                    armed_at,
                    ending: None,
                };
            }
            // A sudo prompt, a tab still starting, the quiet epoch: not an attempt.
            Err(error) if error.is_activity_deferred() => {
                aterm_log::debug!("launch record: the renewal was deferred: {error}");
                self.record_update_switch_stopped(true, &error.to_string());
                self.launch_record_renewal.state =
                    wait_until(crate::automatic_update_activity_retry_at(now));
            }
            Err(error) => {
                aterm_log::warn!("launch record: attempt {k} of {ATTEMPTS} did not start: {error}");
                self.record_update_switch_stopped(k < ATTEMPTS, &error.to_string());
                self.launch_record_renewal.attempts = k;
                self.launch_record_renewal.rearm_after_attempt(now);
            }
        }
    }

    /// A renewal attempt's completion came back with this process still running
    /// (`App::reduce_returned_handoff_completion`; a Commit never returns: the parent
    /// exits). Re-arms it, or ends it when an update took over or the automatic lane went
    /// off. Returns whether its stop is routine: another attempt is armed, or the update
    /// lands by itself.
    pub(crate) fn reduce_returned_launch_record_renewal(
        &mut self,
        lane: crate::app_native::HandoffFailureLane,
        detail: &str,
    ) -> bool {
        let RenewalState::InFlight { ending, .. } = self.launch_record_renewal.state else {
            aterm_log::warn!(
                "launch record: a renewal attempt returned with none in flight ({lane:?}): \
                 {detail}"
            );
            return false;
        };
        if let Some(end) = ending {
            self.settle_launch_record_renewal(end.settles());
            return matches!(end, InFlightEnd::LeaveToUpdate);
        }
        let k = self.launch_record_renewal.attempts;
        aterm_log::info!(
            "launch record: renewal attempt {k} of {ATTEMPTS} returned ({lane:?}): {detail}"
        );
        self.launch_record_renewal
            .rearm_after_attempt(Instant::now());
        k < ATTEMPTS
    }

    /// Ends the renewal and books what `outcome` says, once: the entry whose step is the
    /// person's when no renewal runs or none worked, or a ripening claim of it when an
    /// update's handoff will renew the record (it shows only if that update never lands).
    pub(crate) fn settle_launch_record_renewal(&mut self, outcome: RenewalDecision) {
        let pid = std::process::id();
        let entry = match outcome {
            RenewalDecision::Book(BookCause::NoRenewal(why)) => {
                aterm_log::warn!(
                    "launch record: LaunchServices lists this app with no pid and never as pid \
                     {pid}; System Settings' Quit & Reopen, the menu-bar item and TCC's \
                     attribution miss this process until it quits and is opened again ({})",
                    why.cause()
                );
                crate::update_words::own_launch_record_stale_entry(pid)
            }
            RenewalDecision::Book(BookCause::StillStale { from }) => {
                crate::update_words::own_launch_record_renewed_stale_entry(pid, from)
            }
            RenewalDecision::Book(BookCause::Spent { attempts }) => {
                crate::update_words::own_launch_record_renewal_spent_entry(pid, attempts)
            }
            RenewalDecision::LeaveToUpdate => {
                aterm_log::info!(
                    "launch record: an update is staged; its handoff renews this process's \
                     record as it lands"
                );
                self.launch_record_renewal.state = RenewalState::Ended;
                // Only the claim: an entry open now (adopted from the parent, or kept by an
                // update attempt that returned) would show the step before that update had
                // its chance.
                self.except_end(&crate::update_words::launch_unregistered_entry(0).key());
                self.except_after(
                    &crate::update_words::own_launch_record_stale_entry(pid),
                    Duration::from_secs(aterm_update::PENDING_UPDATE_OVERDUE_SECS),
                );
                return;
            }
            RenewalDecision::Renew | RenewalDecision::Wait(_) => return,
        };
        self.launch_record_renewal.state = RenewalState::Ended;
        self.except(&entry);
    }

    /// An update was staged (`App::finish_native_update_reconcile`): an attempt in flight
    /// yields to the update lane, whose handoff renews the record as it lands
    /// ([`Self::end_launch_record_renewal_in_flight`]). An armed renewal needs nothing:
    /// its next look answers `LeaveToUpdate`.
    pub(crate) fn yield_launch_record_renewal_to_update(&mut self) {
        self.end_launch_record_renewal_in_flight(InFlightEnd::LeaveToUpdate);
    }

    /// `[update] auto_apply` went off (`about_to_wait`'s on-to-off edge): nothing replaces
    /// this process unasked, so the renewal ends as [`decide`] would rule. An armed one
    /// books the entry now, and an attempt in flight stands down
    /// ([`Self::end_launch_record_renewal_in_flight`]) and books it at its return.
    pub(crate) fn end_launch_record_renewal_for_updates_off(&mut self) {
        match self.launch_record_renewal.state {
            RenewalState::Armed { .. } => self.settle_launch_record_renewal(RenewalDecision::Book(
                BookCause::NoRenewal(NoRenewal::UpdatesOff),
            )),
            RenewalState::InFlight { .. } => {
                self.end_launch_record_renewal_in_flight(InFlightEnd::UpdatesOff);
            }
            RenewalState::Idle | RenewalState::Ended => {}
        }
    }

    /// Ends an attempt in flight at its return with `end` instead of re-arming it (the
    /// first reason stands). One still before its park stands down now: the stand-down
    /// for a worker that holds the successor, and the cancel for one still proving the
    /// bundle or waiting for the dial, which reads nothing else, so it ends there rather
    /// than launch and hold a successor only to stand it down. A parked one finishes
    /// within its freeze budget.
    fn end_launch_record_renewal_in_flight(&mut self, end: InFlightEnd) {
        let RenewalState::InFlight { ending, .. } = &mut self.launch_record_renewal.state else {
            return;
        };
        if ending.is_some() {
            return;
        }
        *ending = Some(end);
        if self.pending_update_handoff.is_some() {
            return;
        }
        self.stand_down_prelaunched_successor(crate::app_update_handoff::HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            detail: end.stand_down_detail().to_string(),
        });
        if let Some(prelaunch) = &self.update_handoff_prelaunch {
            let _ = prelaunch.cancel.try_send(());
        }
    }

    /// Whether the overlap handoff in flight is a renewal's attempt: its record carries
    /// `SameImageHandoff::RenewLaunchRecord`.
    pub(crate) fn launch_record_renewal_in_flight(&self) -> bool {
        let renews = |same_image: Option<crate::app_update_handoff::SameImageHandoff>| {
            same_image
                .is_some_and(crate::app_update_handoff::SameImageHandoff::renews_launch_record)
        };
        self.update_handoff_prelaunch
            .as_ref()
            .is_some_and(|prelaunch| renews(prelaunch.same_image))
            || self
                .pending_update_handoff
                .as_ref()
                .is_some_and(|pending| renews(pending.same_image))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fact for a renewal, the installed bundle observed as this build.
    fn renews() -> RenewalFacts {
        RenewalFacts {
            renewed_from: None,
            auto_apply: true,
            handoff_unavailable: false,
            clean_commit: true,
            update_pending: false,
            installed_build: Some(107),
            current_build: 107,
            update_work: false,
            attempts: 0,
            live_terminal: true,
        }
    }

    #[test]
    fn a_stale_record_renews_only_when_nothing_else_will() {
        use BookCause::{NoRenewal as Off, Spent, StillStale};
        use RenewalDecision::{Book, LeaveToUpdate, Renew, Wait};
        let base = renews();
        let against = RenewalFacts {
            renewed_from: None,
            auto_apply: false,
            handoff_unavailable: true,
            clean_commit: false,
            update_pending: true,
            installed_build: Some(106),
            current_build: 107,
            update_work: true,
            attempts: ATTEMPTS,
            live_terminal: false,
        };
        let rows = [
            (base, Renew),
            // Each fact alone, to the decision it owns.
            (
                RenewalFacts {
                    renewed_from: Some(47362),
                    ..base
                },
                Book(StillStale { from: 47362 }),
            ),
            (
                RenewalFacts {
                    auto_apply: false,
                    ..base
                },
                Book(Off(NoRenewal::UpdatesOff)),
            ),
            (
                RenewalFacts {
                    handoff_unavailable: true,
                    ..base
                },
                Book(Off(NoRenewal::HandoffUnavailable)),
            ),
            (
                RenewalFacts {
                    clean_commit: false,
                    ..base
                },
                Book(Off(NoRenewal::Unstamped)),
            ),
            (
                RenewalFacts {
                    update_pending: true,
                    ..base
                },
                LeaveToUpdate,
            ),
            (
                RenewalFacts {
                    installed_build: Some(106),
                    ..base
                },
                Book(Off(NoRenewal::InstalledIsAnotherBuild)),
            ),
            (
                RenewalFacts {
                    update_work: true,
                    ..base
                },
                Wait(RenewalWait::Work),
            ),
            (
                RenewalFacts {
                    attempts: ATTEMPTS,
                    ..base
                },
                Book(Spent { attempts: ATTEMPTS }),
            ),
            (
                RenewalFacts {
                    live_terminal: false,
                    ..base
                },
                Wait(RenewalWait::Terminal),
            ),
            // Precedence.
            (
                RenewalFacts {
                    renewed_from: Some(47362),
                    ..against
                },
                Book(StillStale { from: 47362 }),
            ),
            (
                RenewalFacts {
                    update_pending: true,
                    installed_build: Some(108),
                    ..base
                },
                LeaveToUpdate,
            ),
            (
                RenewalFacts {
                    update_work: true,
                    attempts: ATTEMPTS,
                    ..base
                },
                Wait(RenewalWait::Work),
            ),
            (
                RenewalFacts {
                    attempts: ATTEMPTS,
                    live_terminal: false,
                    ..base
                },
                Book(Spent { attempts: ATTEMPTS }),
            ),
            (
                RenewalFacts {
                    installed_build: None,
                    ..base
                },
                Renew,
            ),
            (
                RenewalFacts {
                    attempts: ATTEMPTS - 1,
                    ..base
                },
                Renew,
            ),
        ];
        for (facts, decision) in rows {
            assert_eq!(decide(facts), decision, "{facts:?}");
        }
    }
}
