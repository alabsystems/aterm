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
//! preserved, within [`LANDS_WITHIN`] of being armed — on any machine, including
//! one that is never idle. Activity (keystrokes, PTY output, focus) may DELAY the
//! landing inside that bound; it may never disable the lane or convert it to
//! manual-only. Only a genuine physical failure of the handoff may do that, on
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

use std::time::Duration;

/// How long the lane holds out for a machine-wide idle moment — no HID input,
/// no keystroke in an aterm window, every live session's output quiet — before
/// it stops asking for one. Long enough that an ordinary pause wins the race and
/// the update lands invisibly.
pub(crate) const PREFER_IDLE_WINDOW: Duration = Duration::from_secs(120);

/// How long after that the lane still waits for a gap in the terminal's own
/// output while an aterm window is focused (the owner's 2026-09-18 ruling: a
/// stream the user is watching should not skip a beat). Since the late park the
/// freeze that would interrupt it is about 300 ms, measured, so this preference
/// is bounded rather than absolute.
pub(crate) const PREFER_OUTPUT_GAP_WINDOW: Duration = Duration::from_secs(180);

/// How long after THAT the lane still waits for a gap between keystrokes (the
/// one comfort rule with a natural bound: nobody types for ten minutes without
/// a two-and-a-half-second pause). Past it the lane lands regardless; input
/// typed during the freeze is queued and replayed after Commit, never swallowed.
pub(crate) const KEYS_ONLY_WINDOW: Duration = Duration::from_secs(600);

/// The bound every surface quotes: an armed automatic apply parks its readers
/// no later than this after arming, whatever the terminal is doing.
pub(crate) const LANDS_WITHIN: Duration = Duration::from_secs(
    PREFER_IDLE_WINDOW.as_secs() + PREFER_OUTPUT_GAP_WINDOW.as_secs() + KEYS_ONLY_WINDOW.as_secs(),
);

/// Where on the ladder an armed artifact stands, as a function of how long ago
/// it was armed. Monotone in that duration: a phase, once reached, is never
/// left except by the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ApplyPhase {
    /// Park only at a machine-wide idle moment.
    PreferIdle,
    /// Park at a gap between keystrokes, and — while an aterm window is
    /// focused — a gap in every session's output.
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
    /// window inside the typing gap (and no warm-up gesture holds the lane).
    pub(crate) hands_off_keys: bool,
    /// `App::automatic_update_output_quiet`: every live session's PTY output is
    /// at least one quiet epoch old.
    pub(crate) output_quiet: bool,
    /// `App::any_os_window_focused`: an aterm window has keyboard focus, so the
    /// user is looking at this terminal.
    pub(crate) focused: bool,
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
    match phase {
        ApplyPhase::Land => None,
        _ if !facts.hands_off_keys => {
            Some("a keystroke landed in an aterm window inside the typing gap")
        }
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
        AttemptResult::Accepted | AttemptResult::InstalledNeedsRelaunch => {
            AttemptDisposition::Complete
        }
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
        assert_eq!(LANDS_WITHIN, Duration::from_secs(15 * 60));
    }

    /// Each phase weakens exactly one preference, in order, and `Land` refuses
    /// nothing — including the keystroke gap, which the deferred-input queue
    /// makes safe to override.
    #[test]
    fn each_phase_relaxes_exactly_its_own_preference() {
        let calm = ActivityFacts {
            quiet: true,
            hands_off_keys: true,
            output_quiet: true,
            focused: true,
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
