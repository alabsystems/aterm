// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure ordering policy for automatic application of a staged update.
//!
//! The host owns wall-clock instants and the durable updater ledger. This module
//! owns the deterministic decisions that must survive event reordering — a stage
//! wake arms intent even while a manual check is active, active work waits
//! without consuming that intent, physical handoff failures become manual-only —
//! and it owns THE LADDER: the one function that says, for an artifact armed
//! some time ago, what the terminal has to look like before the readers park.
//!
//! # The law (docs/DESIGN-auto-apply-ladder-2026-09-21.md)
//!
//! A verified staged build applies by itself, in place, with every shell
//! preserved, within a minute of being armed ([`LANDS_WITHIN`] plus the
//! [`SWITCH_ALLOWANCE`]) — on any machine, including one that is never idle.
//! Activity (keystrokes, PTY output, focus) may DELAY the landing inside that
//! bound; it may never disable the lane or convert it to manual-only. Only a genuine physical failure of the handoff may do that, on
//! its own bounded, converging budget.
//!
//! The ladder is a preference for a comfortable moment that weakens with time.
//! Before it existed the lane waited for a machine-wide idle moment that a daily
//! driver — an agent streaming into one pane, a human in another app — never
//! offers; every bound placed on that wait was another wait (a grace, then a
//! typing hold, then a stand-down that lapsed thirty minutes later), and on
//! 2026-09-20 the owner's own aterm cycled through all of them eleven times in
//! seven hours without ever landing a verified 0.89.0. The phases below are the
//! same preferences, in the same order, each with a stated end.
//!
//! The ends are SECONDS (2026-09-23). The first ladder spent 120 + 180 + 600 s
//! on them, and on a busy machine v0.91.0 sat "ready" for 203 s — to avoid a
//! freeze of about 0.4 s in which no keystroke is lost (typed input is queued
//! and replayed after Commit). Now an idle machine lands at the first poll, a
//! busy one at its first pause, and nobody waits more than a minute.

use std::time::Duration;

/// How long the lane holds out for a machine-wide idle moment — no HID input,
/// no keystroke in an aterm window, every live session's output quiet — before
/// it stops asking for one. A few quiet-epoch polls: on an idle machine the
/// first one wins and the update lands invisibly; on a busy one it costs
/// seconds, not minutes.
pub(crate) const PREFER_IDLE_WINDOW: Duration = Duration::from_secs(3);

/// How long after that the lane still waits for a gap in the terminal's own
/// output while an aterm window is focused (the owner's 2026-09-18 ruling: a
/// stream the user is watching should not skip a beat). Since the late park the
/// freeze that would interrupt it is about 300-450 ms, measured, so this
/// preference is bounded rather than absolute.
pub(crate) const PREFER_OUTPUT_GAP_WINDOW: Duration = Duration::from_secs(7);

/// How long after THAT the lane still waits for a gap between keystrokes (the
/// one comfort rule with a natural bound: nobody types for most of a minute
/// without a two-and-a-half-second pause). Past it the lane lands regardless;
/// input typed during the freeze is queued and replayed after Commit, never
/// swallowed.
pub(crate) const KEYS_ONLY_WINDOW: Duration = Duration::from_secs(45);

/// When the ladder stops waiting: from this long after arming, the next poll
/// starts the attempt whatever the terminal is doing (a consent warm-up the
/// user started is the one hold, capped by its own setting). The switch itself
/// follows ([`SWITCH_ALLOWANCE`]), so this ends early enough that the build is
/// RUNNING within the minute every surface quotes.
pub(crate) const LANDS_WITHIN: Duration = Duration::from_secs(
    PREFER_IDLE_WINDOW.as_secs() + PREFER_OUTPUT_GAP_WINDOW.as_secs() + KEYS_ONLY_WINDOW.as_secs(),
);

/// What the switch takes once an attempt starts: the next poll (500 ms), the
/// new process's launch (1.3-1.4 s measured on 0.90/0.91), the park and the
/// freeze (0.3-0.45 s), with room for a busier machine.
pub(crate) const SWITCH_ALLOWANCE: Duration = Duration::from_secs(5);

// "Within a minute" is what the surfaces say: the ladder AND the switch after
// it may not outgrow it.
const _: () = assert!(LANDS_WITHIN.as_secs() + SWITCH_ALLOWANCE.as_secs() <= 60);

/// Where on the ladder an armed artifact stands, as a function of how long ago
/// it was armed. Monotone in that duration: a phase, once reached, is never
/// left except by the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ApplyPhase {
    /// Park only at a machine-wide idle moment.
    PreferIdle,
    /// Park at a gap between keystrokes, and — while an aterm window is
    /// focused — a gap in the output of every session shown in the active tab
    /// of a focused OS window.
    PreferOutputGap,
    /// Park at a gap between keystrokes; output is not consulted.
    KeysOnly,
    /// Park now.
    Land,
}

impl ApplyPhase {
    /// The wire spelling (`aterm ctl update status` prints it as `apply_phase=`).
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PreferIdle => "prefer-idle",
            Self::PreferOutputGap => "prefer-output-gap",
            Self::KeysOnly => "keys-only",
            Self::Land => "land",
        }
    }

    /// What the lane is waiting for in this phase, for the log line that
    /// announces the phase.
    #[must_use]
    pub(crate) fn waits_for(self) -> &'static str {
        match self {
            Self::PreferIdle => "a quiet moment (no input, no output)",
            Self::PreferOutputGap => {
                "a gap in typing and, while aterm is focused, in terminal output"
            }
            Self::KeysOnly => "a gap in typing only; streaming output no longer defers it",
            Self::Land => "nothing: it lands at the next poll",
        }
    }
}

/// THE LADDER. `since_armed` is how long ago the artifact was armed.
#[must_use]
pub(crate) fn apply_phase(since_armed: Duration) -> ApplyPhase {
    if since_armed < PREFER_IDLE_WINDOW {
        ApplyPhase::PreferIdle
    } else if since_armed < PREFER_IDLE_WINDOW + PREFER_OUTPUT_GAP_WINDOW {
        ApplyPhase::PreferOutputGap
    } else if since_armed < LANDS_WITHIN {
        ApplyPhase::KeysOnly
    } else {
        ApplyPhase::Land
    }
}

/// The facts about the terminal that the ladder reads, sampled at one instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActivityFacts {
    /// `App::automatic_update_activity_quiet`: one quiet epoch since the last
    /// input, PTY output or structural event, machine-wide.
    pub(crate) quiet: bool,
    /// `App::update_apply_hands_off_keys`: no keystroke landed in an aterm
    /// window inside the typing gap.
    pub(crate) hands_off_keys: bool,
    /// `App::automatic_update_output_quiet`: every session shown in the active
    /// tab of a focused OS window has PTY output at least one quiet epoch old
    /// (a background tab, an unfocused window or a zoomed-away split does not
    /// count).
    pub(crate) output_quiet: bool,
    /// `App::any_os_window_focused`: an aterm window has keyboard focus, so the
    /// user is looking at this terminal.
    pub(crate) focused: bool,
    /// `App::update_apply_warmup_holds`: a folder-access warm-up the user
    /// started is still running, so a macOS consent dialog may be on screen.
    /// Refused in EVERY phase, `Land` included: it is the user's own gesture,
    /// capped by `[privacy] warmup_hold_ms`, and landing on top of a dialog
    /// they asked for would throw away their answer.
    pub(crate) consent_warmup: bool,
    /// `HostHandle::restored_pending`: a cold restore's agents are still being
    /// relaunched in the tabs it reopened (round four of the 2026-09 update
    /// robustness work, plan item 7). Refused in EVERY phase, `Land`
    /// included, like the warm-up and for the same reason: it is a bounded
    /// hold that is not the terminal's activity — a queue of seconds, capped
    /// at `harness_host::RESTORED_HOLD` from the first relaunch and once per
    /// process — and landing in the middle of it interrupts relaunches the
    /// reopened layout's row just promised. Past the cap the update lands
    /// and the successor carries the rest.
    pub(crate) harness_restored_pending: bool,
}

/// The refusal that means only the person's TYPING holds the park.
const TYPING_REFUSAL: &str = "a keystroke landed in an aterm window inside the typing gap";

/// The refusal while a cold restore's agents are still being relaunched
/// ([`ActivityFacts::harness_restored_pending`]).
pub(crate) const RESTORED_PENDING_REFUSAL: &str =
    "the agents of the tabs a restore reopened are still being relaunched";

/// The one log line the restored agents' hold earns at a poll of the automatic
/// lane for `build`, if any: when `holding` begins it says so — the hold and its
/// bound — and when it ends it says that too; `said` is the ladder's record of
/// which was said last, so each is said once.
///
/// WHY IT IS SAID AT ALL (the round-four review): the hold refuses every phase,
/// `Land` included, for up to [`crate::harness_host::RESTORED_HOLD`], while the
/// phase line had just logged "now waiting for nothing: it lands at the next
/// poll" and each deferral is logged only at debug. Minutes of arm-then-silence
/// past that promise, with nothing saying why, is exactly the shape the ladder's
/// phase lines were written to end. `update status` names the hold too
/// (`apply_held=`).
pub(crate) fn restored_hold_line(build: u64, holding: bool, said: &mut bool) -> Option<String> {
    match (holding, *said) {
        (true, false) => {
            *said = true;
            Some(format!(
                "update auto-apply for build {build}: held while {RESTORED_PENDING_REFUSAL} (at \
                 most {} s from the first relaunch); it lands once they are back",
                crate::harness_host::RESTORED_HOLD.as_secs()
            ))
        }
        (false, true) => {
            *said = false;
            Some(format!(
                "update auto-apply for build {build}: the restored agents' hold is over; it \
                 lands as soon as the ladder allows"
            ))
        }
        _ => None,
    }
}

/// Why the automatic lane will not park in `phase` given `facts`, or `None`
/// when it may. ONE predicate for the two places that ask — the entry
/// (`apply_native_update`, before a successor is launched) and the park
/// (`prelaunch_park_admitted`, with the successor booted and holding) — each
/// re-reading the facts at its own instant. Explicit lanes never ask.
#[must_use]
pub(crate) fn automatic_park_refusal(
    phase: ApplyPhase,
    facts: ActivityFacts,
) -> Option<&'static str> {
    if facts.consent_warmup {
        return Some("a folder-access warm-up the user started is still running");
    }
    if facts.harness_restored_pending {
        return Some(RESTORED_PENDING_REFUSAL);
    }
    match phase {
        ApplyPhase::Land => None,
        _ if !facts.hands_off_keys => Some(TYPING_REFUSAL),
        ApplyPhase::PreferIdle if !facts.quiet => {
            Some("terminal input/output is still inside the quiet epoch")
        }
        ApplyPhase::PreferOutputGap if facts.focused && !facts.output_quiet => {
            Some("terminal output is still streaming")
        }
        ApplyPhase::PreferIdle | ApplyPhase::PreferOutputGap | ApplyPhase::KeysOnly => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArmFacts {
    pub(crate) enabled: bool,
    pub(crate) current_build: u64,
    pub(crate) armed_build: Option<u64>,
    /// True only when `armed_build` names the same build and exact DMG digest.
    pub(crate) armed_exact: bool,
    /// A physical failure latched this exact artifact manual-only.
    pub(crate) manual_only_exact: bool,
    /// Build of the sticky manual-only artifact, if any. Older wakes are stale.
    pub(crate) manual_only_build: Option<u64>,
    pub(crate) incoming_build: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArmDecision {
    Clear,
    Keep,
    SuppressManualOnly,
    Set(u64),
}

/// Reduce one durable stage notification into persistent intent. Active updater
/// work is intentionally not an input: ordering may delay application, never
/// erase knowledge that a newer build arrived.
#[must_use]
pub(crate) fn arm(facts: ArmFacts) -> ArmDecision {
    if !facts.enabled {
        return ArmDecision::Clear;
    }
    if facts.manual_only_exact
        || facts
            .manual_only_build
            .is_some_and(|manual| manual > facts.incoming_build)
    {
        return ArmDecision::SuppressManualOnly;
    }
    if facts.armed_build.is_some_and(|armed| {
        armed > facts.current_build
            && (armed > facts.incoming_build
                || (armed == facts.incoming_build && facts.armed_exact))
    }) {
        return ArmDecision::Keep;
    }
    if facts.incoming_build <= facts.current_build {
        return ArmDecision::Clear;
    }
    ArmDecision::Set(facts.incoming_build)
}

/// What the lane knows, at one look, about a STRUCTURAL latch — automatic
/// apply converged on build N's bytes after two handoffs the bytes answered
/// for (gap 14, 2026-09-26). The host owns the clock, the disk and the boot
/// sentinel; this is the decision over what they said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StructuralLatchFacts {
    /// The latch's ONE re-sample is still owed and its deadline, a day after
    /// convergence, has passed.
    pub(crate) resample_due: bool,
    /// The latch's ONE re-sample is still owed, due or not: the convergence
    /// notice is promising it ("it tries again by itself in 24 h").
    pub(crate) resample_owed: bool,
    /// A verified download strictly newer than the latched ACTIVATION that has
    /// not yet been offered the install path, if one is on disk. The host
    /// fills it only for a latch on the installed activation: on the download
    /// side a newer download is imported as the stage and arms by itself.
    pub(crate) unspent_newer_download: Option<u64>,
    /// A verified download strictly newer than the latched activation is on
    /// disk, but a retire for it was REFUSED and its retry deadline has not
    /// passed (round three review): not offered now, and still outranking the
    /// day's re-sample — the latched build is not launched while a newer one is
    /// on its way. The host fills it for the activation only, like
    /// [`Self::unspent_newer_download`].
    pub(crate) newer_download_waiting: bool,
    /// Whether one more launch of the latched build keeps its boot trial under
    /// the revert threshold: `Some(false)` when that launch would be the one
    /// `check_boot_health` reverts on, `None` when the sentinel's count has not
    /// been measured yet.
    pub(crate) trial_room: Option<bool>,
}

/// The answer [`structural_latch`] gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StructuralLatchDecision {
    /// Nothing new: the latch stands as it is.
    Hold,
    /// Release the latch for ONE automatic attempt at the latched build: its
    /// one re-sample came due and the boot trial has room for the launch.
    Release,
    /// The re-sample is due but would be the launch the boot trial reverts on,
    /// or it is still PROMISED on a trial measured with no room: the latch
    /// stays, loses its deadline, and the host SAYS so. Every launch left is
    /// the reverting one, so a later look could only say the same again.
    HoldForTrial,
    /// The re-sample is due and the trial has not been measured: decide again
    /// at the next observation, spending nothing.
    Unmeasured,
    /// A verified download strictly newer than the latched activation is on
    /// disk: RETIRE the activation from the install path so the newer build
    /// applies over the running one (`aterm_update::supersede_installed_
    /// activation`, then the ordinary staged lane). Whatever the trial's room:
    /// it launches nothing, so it spends no launch of the latched build — and
    /// it takes precedence over a due re-sample of it, which would launch the
    /// older build while a newer one waits.
    Supersede { newer: u64 },
}

/// THE STRUCTURAL LATCH'S WAY OUT (gap 14, 2026-09-26; round three of the 2026-09
/// update robustness work).
///
/// A structural convergence used to be `retry_at: None`, and on the installed
/// ACTIVATION — where every launched-lane failure lands — nothing automatic
/// ever moved it. Two events now move it:
///
/// * a verified download strictly newer than the latched activation SUPERSEDES
///   it. Gap 14 answered a newer release with one more attempt at the latched
///   build, "whose successor applies the newer stage" — but that successor is
///   the handoff's authorized target and refuses to boot-apply past itself
///   (`aterm_update::apply_staged_if_ready`), so the attempt only ever re-ran
///   the handoff that had already failed twice, walked the older build's boot
///   trial one launch closer to its revert, and — once the trial had no room —
///   held the newer release forever. The newer build is now the candidate
///   itself: the running build takes the install path back and the ordinary
///   staged lane applies the newer one over it, with a trial of its own;
/// * the latch's ONE re-sample coming due a day after convergence earns one
///   more automatic attempt at the latched build — never at the boot trial's
///   expense: a structural `ChildDied` keeps its counted launch, so after two
///   of them the next launch is the one the sentinel reverts on. An automatic
///   attempt must not be what spends it — that is the promise
///   `STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS < MAX_BOOT_ATTEMPTS` makes — so a
///   re-sample the trial cannot afford is held, and said; nor is it PROMISED
///   past the first count that rules it out (gap 14 review, 2026-09-26).
///
/// A retire the disk or the moment REFUSED gives its release back, to be
/// offered again at a retry deadline (round three review); while it waits the
/// release still outranks the day, and a promise the trial cannot afford is
/// withdrawn all the same.
///
/// With neither, the latch stands: the convergence notice has said so, and the
/// Version menu's apply is the press that moves it.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "NativeUpdateStructuralLatch",
        action = "Decide",
        project = "aterm_gui::native_updater_conformance::project_structural_latch"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "TrialHasRoom",
        reason = "The boot sentinel's measured count, read on the updater facts worker (`InstalledUpdate::trial_launches`); an input to this decision, not a transition of the shipping reducer."
    )
)]
#[cfg_attr(
    test,
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "TrialIsSpent",
        reason = "The boot sentinel's measured count at the revert threshold; an input to this decision, not a transition of the shipping reducer."
    )
)]
#[cfg_attr(
    test,
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "DayPasses",
        reason = "The monotonic clock passing the re-sample deadline; the host reads it into `resample_due`."
    )
)]
#[cfg_attr(
    test,
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "NewerArrives",
        reason = "The checker staging a verified newer download on disk; the host reads it into `unspent_newer_download`."
    )
)]
#[cfg_attr(
    test,
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "RetryDue",
        reason = "The monotonic clock passing a refused retire's retry deadline (`AutoApplyStructuralVerdict::newer_retry`); the host reads it into `unspent_newer_download` / `newer_download_waiting`."
    )
)]
#[cfg_attr(
    test,
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "TrialUnreadable",
        reason = "The installed facts ceasing to report the latched build (the facts worker drops a yanked bundle newer than the running one); the host reads it as `trial_room: None`, and its bound on such looks is `App::look_at_structural_latch`'s."
    )
)]
#[cfg_attr(
    test,
    aterm_spec::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "TrialReadAgain",
        reason = "The installed facts reporting the latched build again; the host reads it into `trial_room`, and a look that finds it clears the unread count."
    )
)]
#[must_use]
pub(crate) fn structural_latch(facts: StructuralLatchFacts) -> StructuralLatchDecision {
    if let Some(newer) = facts.unspent_newer_download {
        return StructuralLatchDecision::Supersede { newer };
    }
    if facts.newer_download_waiting {
        // A newer build is on its way: the day's re-sample of the older one
        // waits behind it, whatever the trial — but a re-sample promised on a
        // trial measured with no room is withdrawn now, as it would be with
        // nothing newer on disk.
        return if facts.resample_owed && facts.trial_room == Some(false) {
            StructuralLatchDecision::HoldForTrial
        } else {
            StructuralLatchDecision::Hold
        };
    }
    if !facts.resample_due {
        // Nothing earned — but a re-sample still promised on a trial measured
        // with no room is a promise nothing will keep.
        return if facts.resample_owed && facts.trial_room == Some(false) {
            StructuralLatchDecision::HoldForTrial
        } else {
            StructuralLatchDecision::Hold
        };
    }
    match facts.trial_room {
        None => StructuralLatchDecision::Unmeasured,
        Some(false) => StructuralLatchDecision::HoldForTrial,
        Some(true) => StructuralLatchDecision::Release,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PollFacts {
    pub(crate) enabled: bool,
    pub(crate) deadline_ready: bool,
    pub(crate) current_build: u64,
    pub(crate) target_build: u64,
    pub(crate) work_active: bool,
    pub(crate) applying: bool,
    /// The terminal has observed neither user input nor PTY output for the
    /// host's short monotonic quiet epoch, and no undispatched OS input/output
    /// latch is pending.
    pub(crate) activity_quiet: bool,
    /// Where the artifact stands on the ladder ([`apply_phase`]). In
    /// [`ApplyPhase::PreferIdle`] activity defers; in every later phase the
    /// poll attempts and the finer gates decide at the park.
    pub(crate) phase: ApplyPhase,
    pub(crate) staged_ready: bool,
    pub(crate) staged_build: Option<u64>,
    /// Exact build+DMG identity match between the retained intent and reducer stage.
    pub(crate) staged_exact_target: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitReason {
    Deadline,
    WorkActive,
    Activity,
    StagePending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PollDecision {
    Clear,
    Wait(WaitReason),
    /// Apply this exact staged artifact now. `quiet` reports whether the
    /// machine was actually idle at the poll; `phase` is the ladder phase the
    /// attempt is authorized under, which the host carries to the park gate
    /// through the apply mode.
    Attempt {
        build: u64,
        quiet: bool,
        phase: ApplyPhase,
    },
}

/// Decide one event-loop poll. Every `Wait` retains the caller-owned intent.
#[must_use]
pub(crate) fn poll(facts: PollFacts) -> PollDecision {
    if !facts.enabled || facts.current_build >= facts.target_build {
        return PollDecision::Clear;
    }
    if !facts.deadline_ready {
        return PollDecision::Wait(WaitReason::Deadline);
    }
    if facts.work_active || facts.applying {
        return PollDecision::Wait(WaitReason::WorkActive);
    }
    // Prefer a quiet moment, but only for the first phase. Waiting past it is
    // indistinguishable from never updating.
    if facts.phase == ApplyPhase::PreferIdle && !facts.activity_quiet {
        return PollDecision::Wait(WaitReason::Activity);
    }
    let Some(staged_build) = facts.staged_build else {
        return PollDecision::Wait(WaitReason::StagePending);
    };
    if !facts.staged_ready || staged_build != facts.target_build || !facts.staged_exact_target {
        return PollDecision::Wait(WaitReason::StagePending);
    }
    PollDecision::Attempt {
        build: staged_build,
        quiet: facts.activity_quiet,
        phase: facts.phase,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptResult {
    Accepted,
    #[cfg(any(unix, test))]
    InstalledNeedsRelaunch,
    Blocked,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptDisposition {
    Complete,
    Retry,
    ManualOnly,
}

/// Only cheap native-state blocks receive a bounded retry. A returned physical
/// handoff failure may already have parked readers and touched updater artifacts;
/// it is manual-only so no timer can repeat expensive process work indefinitely.
#[must_use]
pub(crate) fn finish(result: AttemptResult) -> AttemptDisposition {
    match result {
        AttemptResult::Accepted => AttemptDisposition::Complete,
        #[cfg(any(unix, test))]
        AttemptResult::InstalledNeedsRelaunch => AttemptDisposition::Complete,
        AttemptResult::Blocked => AttemptDisposition::Retry,
        AttemptResult::Failed => AttemptDisposition::ManualOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_wake_arms_independently_of_work_ordering() {
        assert_eq!(
            arm(ArmFacts {
                enabled: true,
                current_build: 10,
                armed_build: None,
                armed_exact: false,
                manual_only_exact: false,
                manual_only_build: None,
                incoming_build: 11,
            }),
            ArmDecision::Set(11)
        );
        assert_eq!(
            arm(ArmFacts {
                enabled: true,
                current_build: 10,
                armed_build: Some(12),
                armed_exact: false,
                manual_only_exact: false,
                manual_only_build: None,
                incoming_build: 11,
            }),
            ArmDecision::Keep
        );
        assert_eq!(
            arm(ArmFacts {
                enabled: true,
                current_build: 10,
                armed_build: Some(12),
                armed_exact: false,
                manual_only_exact: false,
                manual_only_build: None,
                incoming_build: 9,
            }),
            ArmDecision::Keep,
            "an out-of-order stale wake cannot erase a newer pending intent"
        );

        assert_eq!(
            arm(ArmFacts {
                enabled: true,
                current_build: 10,
                armed_build: None,
                armed_exact: false,
                manual_only_exact: true,
                manual_only_build: Some(11),
                incoming_build: 11,
            }),
            ArmDecision::SuppressManualOnly,
            "a duplicate wake cannot reset a physical-failure retry budget"
        );
        assert_eq!(
            arm(ArmFacts {
                enabled: true,
                current_build: 10,
                armed_build: Some(11),
                armed_exact: false,
                manual_only_exact: false,
                manual_only_build: None,
                incoming_build: 11,
            }),
            ArmDecision::Set(11),
            "same build with a different exact artifact replaces stale intent"
        );
    }

    #[test]
    fn active_work_waits_and_completed_stage_attempts() {
        let base = PollFacts {
            enabled: true,
            deadline_ready: true,
            current_build: 10,
            target_build: 11,
            work_active: true,
            applying: false,
            activity_quiet: true,
            phase: ApplyPhase::PreferIdle,
            staged_ready: false,
            staged_build: None,
            staged_exact_target: false,
        };
        assert_eq!(poll(base), PollDecision::Wait(WaitReason::WorkActive));
        assert_eq!(
            poll(PollFacts {
                work_active: false,
                activity_quiet: false,
                staged_ready: true,
                staged_build: Some(11),
                staged_exact_target: true,
                ..base
            }),
            PollDecision::Wait(WaitReason::Activity),
            "typing/output defers automatic park without consuming intent"
        );
        assert_eq!(
            poll(PollFacts {
                work_active: false,
                staged_ready: true,
                staged_build: Some(11),
                staged_exact_target: true,
                ..base
            }),
            PollDecision::Attempt {
                build: 11,
                quiet: true,
                phase: ApplyPhase::PreferIdle,
            }
        );
        assert_eq!(
            poll(PollFacts {
                work_active: false,
                staged_ready: true,
                staged_build: Some(11),
                staged_exact_target: false,
                ..base
            }),
            PollDecision::Wait(WaitReason::StagePending),
            "an intent never silently transfers to different bytes under one build"
        );
    }

    /// THE REGRESSION THE LADDER EXISTS FOR: a machine that is never idle used
    /// to defer forever (and later, to stand down and re-arm forever). Past the
    /// first phase the same busy facts must attempt, reporting the phase so the
    /// host takes the lane whose park gate reads the ladder.
    #[test]
    fn a_never_quiet_machine_still_attempts_once_the_idle_phase_ends() {
        let busy = PollFacts {
            enabled: true,
            deadline_ready: true,
            current_build: 10,
            target_build: 11,
            work_active: false,
            applying: false,
            activity_quiet: false,
            phase: ApplyPhase::PreferIdle,
            staged_ready: true,
            staged_build: Some(11),
            staged_exact_target: true,
        };
        assert_eq!(poll(busy), PollDecision::Wait(WaitReason::Activity));
        for phase in [
            ApplyPhase::PreferOutputGap,
            ApplyPhase::KeysOnly,
            ApplyPhase::Land,
        ] {
            assert_eq!(
                poll(PollFacts { phase, ..busy }),
                PollDecision::Attempt {
                    build: 11,
                    quiet: false,
                    phase,
                },
                "{phase:?}"
            );
        }
        // A later phase is not a licence to skip any OTHER gate: it relaxes
        // the idleness preference and nothing else.
        for stalled in [
            PollFacts {
                work_active: true,
                ..busy
            },
            PollFacts {
                applying: true,
                ..busy
            },
            PollFacts {
                staged_exact_target: false,
                ..busy
            },
            PollFacts {
                staged_build: None,
                staged_ready: false,
                ..busy
            },
            PollFacts {
                deadline_ready: false,
                ..busy
            },
        ] {
            assert!(
                matches!(
                    poll(PollFacts {
                        phase: ApplyPhase::Land,
                        ..stalled
                    }),
                    PollDecision::Wait(_)
                ),
                "the ladder only relaxes activity"
            );
        }
        assert_eq!(
            poll(PollFacts {
                enabled: false,
                phase: ApplyPhase::Land,
                ..busy
            }),
            PollDecision::Clear,
            "no phase ever revives a disabled automatic lane"
        );
    }

    /// The ladder is monotone in time and its phases tile the bound exactly.
    #[test]
    fn the_ladder_is_monotone_and_ends_at_the_bound() {
        let edges = [
            (Duration::ZERO, ApplyPhase::PreferIdle),
            (
                PREFER_IDLE_WINDOW - Duration::from_millis(1),
                ApplyPhase::PreferIdle,
            ),
            (PREFER_IDLE_WINDOW, ApplyPhase::PreferOutputGap),
            (
                PREFER_IDLE_WINDOW + PREFER_OUTPUT_GAP_WINDOW - Duration::from_millis(1),
                ApplyPhase::PreferOutputGap,
            ),
            (
                PREFER_IDLE_WINDOW + PREFER_OUTPUT_GAP_WINDOW,
                ApplyPhase::KeysOnly,
            ),
            (
                LANDS_WITHIN - Duration::from_millis(1),
                ApplyPhase::KeysOnly,
            ),
            (LANDS_WITHIN, ApplyPhase::Land),
            (LANDS_WITHIN * 100, ApplyPhase::Land),
        ];
        let mut last = ApplyPhase::PreferIdle;
        for (since, want) in edges {
            let got = apply_phase(since);
            assert_eq!(got, want, "{since:?}");
            assert!(got >= last, "monotone at {since:?}");
            last = got;
        }
        assert_eq!(LANDS_WITHIN, Duration::from_secs(55));
        assert!(
            LANDS_WITHIN + SWITCH_ALLOWANCE <= Duration::from_secs(60),
            "the attempt the ladder starts at its end still lands inside the minute"
        );
    }

    /// Each phase weakens exactly one preference, in order, and `Land` refuses
    /// nothing the terminal does — including the keystroke gap, which the
    /// deferred-input queue makes safe to override. The one refusal no phase
    /// relaxes is the user's own consent warm-up.
    #[test]
    fn each_phase_relaxes_exactly_its_own_preference() {
        let calm = ActivityFacts {
            quiet: true,
            hands_off_keys: true,
            output_quiet: true,
            focused: true,
            consent_warmup: false,
            harness_restored_pending: false,
        };
        for phase in [
            ApplyPhase::PreferIdle,
            ApplyPhase::PreferOutputGap,
            ApplyPhase::KeysOnly,
            ApplyPhase::Land,
        ] {
            assert_eq!(
                automatic_park_refusal(phase, calm),
                None,
                "{phase:?} on a calm machine"
            );
        }
        let typing = ActivityFacts {
            hands_off_keys: false,
            ..calm
        };
        for phase in [
            ApplyPhase::PreferIdle,
            ApplyPhase::PreferOutputGap,
            ApplyPhase::KeysOnly,
        ] {
            assert!(
                automatic_park_refusal(phase, typing).is_some(),
                "{phase:?} never lands mid-word"
            );
        }
        assert_eq!(automatic_park_refusal(ApplyPhase::Land, typing), None);

        let busy_but_hands_off = ActivityFacts {
            quiet: false,
            ..calm
        };
        assert!(automatic_park_refusal(ApplyPhase::PreferIdle, busy_but_hands_off).is_some());
        assert_eq!(
            automatic_park_refusal(ApplyPhase::PreferOutputGap, busy_but_hands_off),
            None,
            "past the idle phase, machine-wide quiet is no longer required"
        );

        let watched_stream = ActivityFacts {
            quiet: false,
            output_quiet: false,
            ..calm
        };
        assert_eq!(
            automatic_park_refusal(ApplyPhase::PreferOutputGap, watched_stream),
            Some("terminal output is still streaming")
        );
        assert_eq!(
            automatic_park_refusal(
                ApplyPhase::PreferOutputGap,
                ActivityFacts {
                    focused: false,
                    ..watched_stream
                }
            ),
            None,
            "with the user in another app the stall is invisible"
        );
        assert_eq!(
            automatic_park_refusal(ApplyPhase::KeysOnly, watched_stream),
            None,
            "the streaming machine this ladder exists for lands here"
        );

        // A warm-up the user started holds every phase — `Land` included, or a
        // landing could arrive while its consent dialog is up.
        let warming = ActivityFacts {
            consent_warmup: true,
            ..calm
        };
        for phase in [
            ApplyPhase::PreferIdle,
            ApplyPhase::PreferOutputGap,
            ApplyPhase::KeysOnly,
            ApplyPhase::Land,
        ] {
            assert_eq!(
                automatic_park_refusal(phase, warming),
                Some("a folder-access warm-up the user started is still running"),
                "{phase:?}"
            );
        }

        // A cold restore's agents still being relaunched hold every phase too
        // — `Land` included (round four, plan item 7): the landing used to
        // drop the queue right after the reopened layout's row said they
        // resume. RED before the change: there was no such fact, and `Land`
        // parked over the queue. Bounded by the host, never by the ladder
        // (`harness_host::RESTORED_HOLD`).
        let relaunching = ActivityFacts {
            harness_restored_pending: true,
            ..calm
        };
        for phase in [
            ApplyPhase::PreferIdle,
            ApplyPhase::PreferOutputGap,
            ApplyPhase::KeysOnly,
            ApplyPhase::Land,
        ] {
            assert_eq!(
                automatic_park_refusal(phase, relaunching),
                Some(RESTORED_PENDING_REFUSAL),
                "{phase:?}"
            );
        }
        // Everything the ladder relaxes at `Land` — keys, output, focus — still
        // does not release it; only the queue emptying or its bound does.
        let relaunching_busy = ActivityFacts {
            quiet: false,
            hands_off_keys: false,
            output_quiet: false,
            ..relaunching
        };
        assert_eq!(
            automatic_park_refusal(ApplyPhase::Land, relaunching_busy),
            Some(RESTORED_PENDING_REFUSAL)
        );
        assert_eq!(
            automatic_park_refusal(
                ApplyPhase::Land,
                ActivityFacts {
                    harness_restored_pending: false,
                    ..relaunching_busy
                }
            ),
            None,
            "the queue done, the landing is at once"
        );
    }

    #[test]
    fn physical_failure_is_manual_only_and_installed_is_complete() {
        assert_eq!(
            finish(AttemptResult::Accepted),
            AttemptDisposition::Complete
        );
        assert_eq!(
            finish(AttemptResult::InstalledNeedsRelaunch),
            AttemptDisposition::Complete
        );
        assert_eq!(finish(AttemptResult::Blocked), AttemptDisposition::Retry);
        assert_eq!(
            finish(AttemptResult::Failed),
            AttemptDisposition::ManualOnly
        );
    }
}
