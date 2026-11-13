// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{HandoffUnavailable, update_admission_refusal};
use crate::native_update_admission::{AdmissionBlock, AdmissionDecision, AdmissionFacts, classify};

fn live_facts() -> AdmissionFacts {
    AdmissionFacts {
        staged_verified: true,
        seamless_capable: false,
        native_state_certified: true,
        live_ptys: 3,
        foreground_jobs: 2,
        unknown_foregrounds: 0,
        image_kept: false,
    }
}

#[test]
fn live_session_refusal_retains_exact_handoff_cause_and_remedy() {
    for unavailable in [
        HandoffUnavailable::UnownedControlSocket,
        HandoffUnavailable::Headless,
    ] {
        let (cause, remedy) = (unavailable.cause(), unavailable.remedy());
        for (foreground_jobs, unknown_foregrounds) in [(2, 0), (0, 1), (1, 2), (0, 3)] {
            let facts = AdmissionFacts {
                foreground_jobs,
                unknown_foregrounds,
                ..live_facts()
            };
            let AdmissionDecision::Block(reason) = classify(facts) else {
                panic!("live terminals without seamless handoff must block");
            };
            let refusal = update_admission_refusal(reason, facts, Some(unavailable), None);
            assert!(
                refusal.is_refused(),
                "a refusal cannot increment a failure streak"
            );
            let message = refusal.into_message();
            assert!(message.contains(cause), "{facts:?}: {message}");
            assert!(message.contains(remedy), "{facts:?}: {message}");
            assert!(
                message.chars().count() <= 400,
                "durable health truncates at 400 characters"
            );
            assert!(!reason.message(facts).contains(cause));
        }
        let kept = AdmissionFacts {
            live_ptys: 0,
            foreground_jobs: 0,
            image_kept: true,
            ..live_facts()
        };
        assert_eq!(
            classify(kept),
            AdmissionDecision::Block(AdmissionBlock::KeptImageWithoutHandoff)
        );
        let refusal = update_admission_refusal(
            AdmissionBlock::KeptImageWithoutHandoff,
            kept,
            Some(unavailable),
            Some("the sessionless cause"),
        );
        assert!(refusal.is_refused());
        let message = refusal.into_message();
        assert!(message.contains(cause), "{message}");
        assert!(!message.contains("terminal tab"), "{message}");
        assert_eq!(
            classify(AdmissionFacts {
                seamless_capable: true,
                ..kept
            }),
            AdmissionDecision::Apply(crate::native_update_admission::ApplyLane::Seamless),
            "a kept image the handoff can carry with no terminal is never refused"
        );
    }
}

/// A kept image the sessionless handoff cannot carry names that cause, and the next launch.
#[test]
fn a_kept_image_without_a_sessionless_handoff_names_its_cause() {
    let kept = AdmissionFacts {
        live_ptys: 0,
        foreground_jobs: 0,
        image_kept: true,
        ..live_facts()
    };
    let cause = "no open tab is one the new build can restore";
    let message = update_admission_refusal(
        AdmissionBlock::KeptImageWithoutHandoff,
        kept,
        None,
        Some(cause),
    )
    .into_message();
    assert_eq!(
        message,
        format!("The update waits for the next launch: {cause}")
    );
    assert_eq!(
        update_admission_refusal(AdmissionBlock::KeptImageWithoutHandoff, kept, None, None)
            .into_message(),
        AdmissionBlock::KeptImageWithoutHandoff.message(kept),
    );
}

#[test]
fn unrelated_admission_refusals_keep_the_primary_reason() {
    let base = live_facts();
    for (facts, expected) in [
        (
            AdmissionFacts {
                staged_verified: false,
                ..base
            },
            AdmissionBlock::UnverifiedStage,
        ),
        (
            AdmissionFacts {
                native_state_certified: false,
                ..base
            },
            AdmissionBlock::NativeStateUncertified,
        ),
        (
            AdmissionFacts {
                live_ptys: 0,
                foreground_jobs: 0,
                unknown_foregrounds: 1,
                ..base
            },
            AdmissionBlock::ForegroundProbeUnknown,
        ),
        (
            AdmissionFacts {
                foreground_jobs: 4,
                ..base
            },
            AdmissionBlock::LivePtysNeedSeamless,
        ),
    ] {
        assert_eq!(classify(facts), AdmissionDecision::Block(expected));
        let refusal = update_admission_refusal(
            expected,
            facts,
            Some(HandoffUnavailable::Headless),
            Some("a sessionless cause"),
        );
        assert!(refusal.is_refused());
        assert_eq!(refusal.into_message(), expected.message(facts));
    }
    assert_eq!(
        update_admission_refusal(AdmissionBlock::LivePtysNeedSeamless, base, None, None)
            .into_message(),
        AdmissionBlock::LivePtysNeedSeamless.message(base),
    );
}
