// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn facts() -> AdmissionFacts {
    AdmissionFacts {
        staged_verified: true,
        seamless_capable: false,
        native_state_certified: true,
        live_ptys: 0,
        foreground_jobs: 0,
        unknown_foregrounds: 0,
        image_kept: false,
    }
}

#[test]
fn cold_lane_is_only_for_verified_safe_sessionless_state() {
    assert_eq!(classify(facts()), AdmissionDecision::Apply(ApplyLane::Cold));
    assert!(matches!(
        classify(AdmissionFacts {
            staged_verified: false,
            ..facts()
        }),
        AdmissionDecision::Block(AdmissionBlock::UnverifiedStage)
    ));
    assert!(matches!(
        classify(AdmissionFacts {
            seamless_capable: true,
            native_state_certified: false,
            foreground_jobs: 2,
            ..facts()
        }),
        AdmissionDecision::Block(AdmissionBlock::NativeStateUncertified)
    ));
    assert_eq!(
        classify(AdmissionFacts {
            live_ptys: 1,
            ..facts()
        }),
        AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless)
    );
    assert_eq!(
        classify(AdmissionFacts {
            unknown_foregrounds: 1,
            ..facts()
        }),
        AdmissionDecision::Block(AdmissionBlock::ForegroundProbeUnknown),
        "an unreadable foreground with no live PTY fails closed"
    );
    assert_eq!(
        classify(AdmissionFacts {
            live_ptys: 2,
            unknown_foregrounds: 1,
            ..facts()
        }),
        AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless),
        "without the seamless lane every live terminal blocks, its foreground read or \
         not: the refusal that says why the handoff is unavailable (U26)"
    );
    assert_eq!(
        classify(AdmissionFacts {
            seamless_capable: true,
            live_ptys: 1,
            unknown_foregrounds: 1,
            ..facts()
        }),
        AdmissionDecision::Apply(ApplyLane::Seamless),
        "an unprobeable foreground must not veto the LOSSLESS lane: seamless \
         signals nothing, so a foreground job is not a hazard it can realize. \
         Blocking here made one orphaned pty disable automatic update entirely."
    );
    assert_eq!(
        classify(AdmissionFacts {
            seamless_capable: true,
            foreground_jobs: 1,
            ..facts()
        }),
        AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless),
        "inconsistent foreground/session counts fail closed"
    );
    assert_eq!(
        classify(AdmissionFacts {
            seamless_capable: true,
            image_kept: true,
            ..facts()
        }),
        AdmissionDecision::Apply(ApplyLane::Seamless),
        "a kept image with no terminal session hands over when the handoff can"
    );
    assert_eq!(
        classify(AdmissionFacts {
            image_kept: true,
            ..facts()
        }),
        AdmissionDecision::Block(AdmissionBlock::KeptImageWithoutHandoff)
    );
}

fn seen() -> Observation {
    Observation {
        staged_verified: true,
        overlap_available: true,
        sessionless_handoff: true,
        native_state_certified: true,
        live_ptys: 0,
        foreground_jobs: 0,
        unknown_foregrounds: 0,
        image_kept: true,
    }
}

/// 0.145.0, 2026-11-02: a Dock-launched window holding only native tabs was refused
/// 249 times in four hours because the facts called the handoff incapable at zero
/// sessions.
#[test]
fn a_kept_image_with_no_terminal_hands_over_when_the_successor_reads_a_sessionless_grant() {
    assert_eq!(
        classify(AdmissionFacts::observed(seen())),
        AdmissionDecision::Apply(ApplyLane::Seamless)
    );
    assert_eq!(
        classify(AdmissionFacts::observed(Observation {
            sessionless_handoff: false,
            ..seen()
        })),
        AdmissionDecision::Block(AdmissionBlock::KeptImageWithoutHandoff),
        "a successor that cannot read a sessionless grant is never handed one"
    );
    assert_eq!(
        classify(AdmissionFacts::observed(Observation {
            overlap_available: false,
            ..seen()
        })),
        AdmissionDecision::Block(AdmissionBlock::KeptImageWithoutHandoff)
    );
    assert_eq!(
        classify(AdmissionFacts::observed(Observation {
            sessionless_handoff: false,
            image_kept: false,
            ..seen()
        })),
        AdmissionDecision::Apply(ApplyLane::Cold),
        "a process that can replace its image keeps the cold lane at zero sessions"
    );
    for sessionless_handoff in [false, true] {
        assert_eq!(
            classify(AdmissionFacts::observed(Observation {
                sessionless_handoff,
                live_ptys: 2,
                foreground_jobs: 1,
                ..seen()
            })),
            AdmissionDecision::Apply(ApplyLane::Seamless),
            "live sessions hand over whatever the sessionless grant"
        );
    }
    assert!(
        !AdmissionBlock::KeptImageWithoutHandoff
            .message(AdmissionFacts::observed(seen()))
            .contains("terminal tab")
    );
}
