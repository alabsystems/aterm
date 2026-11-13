// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        boot_health_complete: true,
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
            current_build: 11,
            boot_health_complete: false,
            work_active: false,
            staged_ready: true,
            staged_build: Some(11),
            staged_exact_target: true,
            ..base
        }),
        PollDecision::Wait(WaitReason::BootHealth),
        "a due intent is retained until startup confirmation even if the stage converged"
    );
    assert_eq!(
        poll(PollFacts {
            deadline_ready: false,
            boot_health_complete: false,
            work_active: false,
            ..base
        }),
        PollDecision::Wait(WaitReason::Deadline),
        "boot completion never pulls a genuinely future retry forward"
    );
    assert_eq!(
        poll(PollFacts {
            boot_health_complete: false,
            work_active: false,
            staged_ready: true,
            staged_build: Some(11),
            staged_exact_target: true,
            ..base
        }),
        PollDecision::Wait(WaitReason::BootHealth),
        "the next update cannot race this process's own boot-trial proof"
    );
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
        boot_health_complete: true,
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
            boot_health_complete: false,
            ..busy
        },
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

/// `restored_hold_line` announces the start and end edges of the restored-agents hold once
/// each, and `NEGATIVE CONTROL:` repeats at an unchanged edge return `None` with `said`
/// preserved.
#[test]
fn restored_hold_line_announces_each_edge_once() {
    let mut said = false;
    assert_eq!(
        restored_hold_line(11, false, &mut said),
        None,
        "negative control: idle before any hold announces nothing"
    );
    assert!(!said);

    let start = restored_hold_line(11, true, &mut said).expect("hold start announced");
    assert!(said);
    assert!(start.contains("build 11"), "{start:?}");
    assert!(start.contains("held while"), "{start:?}");
    assert!(
        start.contains(&format!(
            "{} s",
            crate::harness_host::RESTORED_HOLD.as_secs()
        )),
        "{start:?}"
    );
    assert_eq!(
        restored_hold_line(11, true, &mut said),
        None,
        "negative control: standing hold is not repeated"
    );
    assert!(said);

    let end = restored_hold_line(11, false, &mut said).expect("hold end announced");
    assert!(!said);
    assert!(end.contains("build 11"), "{end:?}");
    assert!(end.contains("hold is over"), "{end:?}");
    assert_eq!(
        restored_hold_line(11, false, &mut said),
        None,
        "negative control: cleared hold is not repeated"
    );
    assert!(!said);
}
