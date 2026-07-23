// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure ordering policy for automatic application of a staged update.
//!
//! The host owns wall-clock deadlines and the durable updater ledger. This module
//! owns only the deterministic decisions that must survive event reordering: a
//! stage wake arms intent even while a manual check is active, active work waits
//! without consuming that intent, while physical handoff failures become manual-only.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArmFacts {
    pub(crate) enabled: bool,
    pub(crate) current_build: u64,
    pub(crate) armed_build: Option<u64>,
    /// True only when `armed_build` names the same build and exact DMG digest.
    pub(crate) armed_exact: bool,
    /// A previous bounded/physical failure latched this exact artifact manual-only.
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
    /// latch is pending. Automatic apply alone requires this; manual apply does
    /// not use this policy.
    pub(crate) activity_quiet: bool,
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
    Attempt { build: u64 },
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
    if !facts.activity_quiet {
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
            PollDecision::Attempt { build: 11 }
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
