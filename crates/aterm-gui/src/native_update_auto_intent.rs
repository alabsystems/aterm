// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure ordering policy for automatic application of a staged update.

use std::time::Duration;

/// How long the lane holds out for a machine-wide idle moment (no HID input, no keystroke in an aterm window, every live session's output quiet): a few quiet-epoch polls, so an idle machine lands invisibly at the first and a busy one loses seconds.
pub(crate) const PREFER_IDLE_WINDOW: Duration = Duration::from_secs(3);

/// How long after that the lane still waits for a gap in the terminal's own output while an aterm window is focused, so a stream the person is watching does not skip a beat.
pub(crate) const PREFER_OUTPUT_GAP_WINDOW: Duration = Duration::from_secs(7);

/// How long after that the lane still waits for a gap between keystrokes: nobody types for most of a minute without a 2.5 s pause.
pub(crate) const KEYS_ONLY_WINDOW: Duration = Duration::from_secs(45);

/// When the ladder stops waiting: from this long after arming, the next poll starts the attempt whatever the terminal is doing (only a consent warm-up or a restore relaunch still holds it).
pub(crate) const LANDS_WITHIN: Duration = Duration::from_secs(
    PREFER_IDLE_WINDOW.as_secs() + PREFER_OUTPUT_GAP_WINDOW.as_secs() + KEYS_ONLY_WINDOW.as_secs(),
);

/// What the switch takes once an attempt starts: the next poll (500 ms), the new process's launch (1.3-1.4 s measured), the park and the freeze (0.3-0.45 s), with room for a busier machine.
pub(crate) const SWITCH_ALLOWANCE: Duration = Duration::from_secs(5);

const _: () = assert!(LANDS_WITHIN.as_secs() + SWITCH_ALLOWANCE.as_secs() <= 60);

/// Where on the ladder an armed artifact stands, as a function of how long ago it was armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ApplyPhase {
    /// Park only at a machine-wide idle moment.
    PreferIdle,
    /// Park at a gap between keystrokes and, while an aterm window is focused, a gap in the output of every session shown in its active tab.
    PreferOutputGap,
    /// Park at a gap between keystrokes; output is not consulted.
    KeysOnly,
    /// Park now.
    Land,
}

impl ApplyPhase {
    /// The wire spelling (`aterm ctl update status` prints it as `apply_phase=`).
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PreferIdle => "prefer-idle",
            Self::PreferOutputGap => "prefer-output-gap",
            Self::KeysOnly => "keys-only",
            Self::Land => "land",
        }
    }

    /// What the lane is waiting for in this phase, for the log line that announces the phase.
    #[must_use]
    pub(crate) const fn waits_for(self) -> &'static str {
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

/// The ladder: the phase of an artifact armed `since_armed` ago.
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
    /// `App::automatic_update_activity_quiet`: one quiet epoch since the last input, PTY output or structural event, machine-wide.
    pub(crate) quiet: bool,
    /// `App::update_apply_hands_off_keys`: no keystroke landed in an aterm window inside the typing gap.
    pub(crate) hands_off_keys: bool,
    /// `App::automatic_update_output_quiet`: every session shown in the active tab of a focused OS window has PTY output at least one quiet epoch old (a background tab, an unfocused window or a zoomed-away split does not count).
    pub(crate) output_quiet: bool,
    /// `App::any_os_window_focused`: an aterm window has keyboard focus, so the user is looking at this terminal.
    pub(crate) focused: bool,
    /// `App::update_apply_warmup_holds`: a folder-access warm-up the person started is still running, so a macOS consent dialog may be up.
    pub(crate) consent_warmup: bool,
    /// `HostHandle::restored_pending`: a cold restore's agents are still being relaunched in the tabs it reopened.
    pub(crate) harness_restored_pending: bool,
}

/// The refusal that means only the person's typing holds the park.
const TYPING_REFUSAL: &str = "a keystroke landed in an aterm window inside the typing gap";

/// The refusal while a cold restore's agents are still being relaunched ([`ActivityFacts::harness_restored_pending`]).
pub(crate) const RESTORED_PENDING_REFUSAL: &str =
    "the agents of the tabs a restore reopened are still being relaunched";

/// The log line the restored agents' hold earns at a poll for `build`, if any: once when `holding` begins (the hold and its bound) and once when it ends; `said` records which was said last.
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

/// Why the automatic lane will not park in `phase` given `facts`, or `None` when it may.
#[must_use]
pub(crate) const fn automatic_park_refusal(
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
    /// Build of the sticky manual-only artifact, if any.
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

/// Whether arming `incoming_build` owes the candidate's pre-park read (its verdict and the handoff policy it seals) now: whenever it is newer than the running build and nothing armed outranks it, not only when the automatic lane takes it.
#[cfg(any(unix, test))]
#[must_use]
pub(crate) const fn arm_reads_candidate(
    decision: ArmDecision,
    incoming_build: u64,
    current_build: u64,
) -> bool {
    match decision {
        ArmDecision::Set(_) => true,
        ArmDecision::Clear | ArmDecision::SuppressManualOnly => incoming_build > current_build,
        ArmDecision::Keep => false,
    }
}

/// Reduce one durable stage notification into persistent intent.
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

/// What the lane knows, at one look, about a structural latch (automatic apply converged on build N's bytes after two handoffs the bytes answered for).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StructuralLatchFacts {
    /// The latch's one re-sample is still owed and its deadline, a day after convergence, has passed.
    pub(crate) resample_due: bool,
    /// The latch's one re-sample is still owed, due or not: the convergence notice promises it ("it tries again by itself in 24 h").
    pub(crate) resample_owed: bool,
    /// A verified download strictly newer than the latched activation, not yet offered the install path.
    pub(crate) unspent_newer_download: Option<u64>,
    /// A verified download strictly newer than the latched activation is on disk, but its retire was refused and the retry deadline has not passed: not offered now, yet it still outranks the re-sample.
    pub(crate) newer_download_waiting: bool,
    /// Whether one more launch of the latched build keeps its boot trial under the revert threshold: `Some(false)` when that launch would be the one `check_boot_health` reverts on, `None` when the sentinel's count has not been measured yet.
    pub(crate) trial_room: Option<bool>,
}

/// The answer [`structural_latch`] gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StructuralLatchDecision {
    /// Nothing new: the latch stands as it is.
    Hold,
    /// Release the latch for one automatic attempt at the latched build: its re-sample came due and the boot trial has room for the launch.
    Release,
    /// The re-sample is due but would be the launch the boot trial reverts on, or is still promised on a trial measured with no room: the latch stays, loses its deadline, and the host says so.
    HoldForTrial,
    /// The re-sample is due and the trial has not been measured: decide again at the next observation, spending nothing.
    Unmeasured,
    /// A verified download strictly newer than the latched activation is on disk: retire the activation from the install path (`aterm_update::supersede_installed_activation`) so the ordinary staged lane applies the newer build.
    Supersede { newer: u64 },
}

/// The structural latch's way out.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateStructuralLatch",
        action = "Decide",
        project = "aterm_gui::native_updater_conformance::project_structural_latch"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "TrialHasRoom",
        reason = "The boot sentinel's measured count, read on the updater facts worker (`InstalledUpdate::trial_launches`); an input to this decision, not a transition of the shipping reducer."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "TrialIsSpent",
        reason = "The boot sentinel's measured count at the revert threshold; an input to this decision, not a transition of the shipping reducer."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "DayPasses",
        reason = "A day passing on the re-sample deadline, the Mac's sleep counted (`App::count_sleep_toward_update_deadlines`); the host reads it into `resample_due`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "NewerArrives",
        reason = "The checker staging a verified newer download on disk; the host reads it into `unspent_newer_download`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "RetryDue",
        reason = "A refused retire's retry deadline (`AutoApplyStructuralVerdict::newer_retry`) passing, the Mac's sleep counted (`App::count_sleep_toward_update_deadlines`); the host reads it into `unspent_newer_download` / `newer_download_waiting`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "TrialUnreadable",
        reason = "The installed facts ceasing to report the latched build (the facts worker drops a yanked bundle newer than the running one); the host reads it as `trial_room: None`, and its bound on such looks is `App::look_at_structural_latch`'s."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateStructuralLatch",
        action = "TrialUnproven",
        reason = "A facts read that could not prove the installed bundle at all (a passing refusal: `spctl` past its ceiling), which the facts worker reports as `installed_unproven`; the host reads it as `trial_room: None`, and its own bound on such looks is `App::look_over_unproven_reads`'s."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
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
        return if facts.resample_owed && facts.trial_room == Some(false) {
            StructuralLatchDecision::HoldForTrial
        } else {
            StructuralLatchDecision::Hold
        };
    }
    if !facts.resample_due {
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
    /// The terminal has observed neither user input nor PTY output for the host's short monotonic quiet epoch, and no undispatched OS input/output latch is pending.
    pub(crate) activity_quiet: bool,
    /// Where the artifact stands on the ladder ([`apply_phase`]).
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
    /// Apply this exact staged artifact now.
    Attempt {
        build: u64,
        quiet: bool,
        phase: ApplyPhase,
    },
}

/// Decide one event-loop poll.
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

/// Only cheap native-state blocks receive a bounded retry.
#[must_use]
pub(crate) const fn finish(result: AttemptResult) -> AttemptDisposition {
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

    /// Arming reads the candidate's handoff policy even when the lane does not take the stage
    /// (`auto_apply = false`, manual-only), so a person's Install never parks under an unread one.
    #[test]
    fn a_stage_a_person_installs_is_read_when_it_arms() {
        let off = |incoming_build| ArmFacts {
            enabled: false,
            current_build: 10,
            armed_build: None,
            armed_exact: false,
            manual_only_exact: false,
            manual_only_build: None,
            incoming_build,
        };
        assert_eq!(arm(off(11)), ArmDecision::Clear);
        assert!(arm_reads_candidate(arm(off(11)), 11, 10), "auto_apply off");
        let latched = ArmFacts {
            enabled: true,
            manual_only_exact: true,
            manual_only_build: Some(11),
            ..off(11)
        };
        assert_eq!(arm(latched), ArmDecision::SuppressManualOnly);
        assert!(
            arm_reads_candidate(arm(latched), 11, 10),
            "latched manual-only"
        );
        let on = ArmFacts {
            enabled: true,
            ..off(11)
        };
        assert!(
            arm_reads_candidate(arm(on), 11, 10),
            "the automatic lane, as before"
        );
        assert!(!arm_reads_candidate(arm(off(10)), 10, 10));
        assert!(!arm_reads_candidate(arm(off(9)), 9, 10));
        let kept = ArmFacts {
            armed_build: Some(12),
            ..on
        };
        assert_eq!(arm(kept), ArmDecision::Keep);
        assert!(!arm_reads_candidate(arm(kept), 11, 10));
    }

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

    /// Past the first phase a never-idle machine attempts, reporting the phase the park gate reads.
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
        assert!(
            LANDS_WITHIN + SWITCH_ALLOWANCE <= Duration::from_secs(60),
            "the attempt the ladder starts at its end still lands inside the minute"
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
