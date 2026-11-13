// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

macro_rules! cause_test {
    ($name:ident, $cause:ident, $word:literal) => {
        #[test]
        fn $name() {
            use aterm_types::network_state::{RepairCause, RepairOutcome};
            let mut repair = crate::control::Repair::default();
            repair.application_failed(crate::Error::repair(
                "private path, key and record fixture",
                RepairCause::$cause,
            ));
            let mut round = attempted();
            round.application_error = repair.application_error().map(bounded_failure);
            let (_, observation, _) = round.observation().unwrap();
            assert_eq!(
                observation.outcome,
                RepairOutcome::Failed(RepairCause::$cause)
            );
            assert_eq!(observation.cause().unwrap().word(), $word);
            assert_eq!(RepairCause::from_word($word), Some(RepairCause::$cause));
            assert!(!format!("{observation:?}").contains("private"));
            repair.begin_round();
            assert!(repair.application_cause().is_none());
        }
    };
}
cause_test!(
    records_legacy_past_crossing,
    LegacyPastCrossing,
    "legacy-past-crossing"
);
cause_test!(
    records_table_undeclared,
    TableUndeclared,
    "table-undeclared"
);
cause_test!(records_quota, Quota, "quota");
cause_test!(records_io, Io, "io");
cause_test!(records_decode, Decode, "decode");
cause_test!(records_not_ready, NotReady, "not-ready");
cause_test!(records_needs_update, NeedsUpdate, "needs-update");
cause_test!(records_lan_held, LanHeld, "lan-held");
cause_test!(records_unreachable, Unreachable, "unreachable");
cause_test!(records_other, Other, "other");

fn attempted() -> RepairRound {
    let mut round = RepairRound::panicked(Some("127.0.0.1:41384".parse().unwrap()));
    round.actor = Some([7; 48]);
    round.exchange_started = true;
    round.phase = Some(crate::control::Phase::Application);
    round.control = Ok(Default::default());
    round.completed_at_ms = 1000;
    round
}

fn failure(cause: aterm_types::network_state::RepairCause) -> Failure {
    Failure::new(cause, "private fixture error and payload")
}

#[test]
fn repair_diagnostics_keep_failures_and_partial_counts_without_error_text() {
    use aterm_types::network_state::{RepairCause, RepairOutcome, RepairStage};
    let mut round = attempted();
    round.application_error = Some(failure(RepairCause::Io));
    round.application_interrupted = Some(crate::replica::Report {
        table: Some([2; 32]),
        sent: 3,
        received: 4,
        deferred: 5,
        ..Default::default()
    });
    let (actor, observation, _) = round.observation().unwrap();
    assert_eq!(actor, [7; 48]);
    assert_eq!(observation.outcome, RepairOutcome::Failed(RepairCause::Io));
    assert_eq!(observation.stage, Some(RepairStage::Application));
    assert_eq!(observation.table, Some([2; 32]));
    assert_eq!(observation.sent, Some(3));
    assert_eq!(observation.received, Some(4));
    assert_eq!(observation.deferred, Some(5));
    assert_eq!(observation.completed_at_ms, 1000);
    assert!(observation.outbound);
    assert!(!format!("{observation:?}").contains("private fixture"));
    for change in [
        (None, true, 1000),
        (Some([7; 48]), false, 1000),
        (Some([7; 48]), true, 0),
    ] {
        let mut invalid = round.clone();
        (
            invalid.actor,
            invalid.exchange_started,
            invalid.completed_at_ms,
        ) = change;
        assert!(invalid.observation().is_none());
    }
}

#[test]
fn an_authenticated_round_does_not_imply_an_application_exchange() {
    use aterm_types::network_state::{RepairCause, RepairOutcome, RepairStage};
    let mut round = attempted();
    assert_eq!(
        round.observation().unwrap().1.outcome,
        RepairOutcome::NotObserved
    );
    round.application = Some(Default::default());
    assert_eq!(
        round.observation().unwrap().1.outcome,
        RepairOutcome::NoCommonTable
    );
    round.application.as_mut().unwrap().table = Some([1; 32]);
    assert_eq!(
        round.observation().unwrap().1.outcome,
        RepairOutcome::Exchanged
    );
    round.application.as_mut().unwrap().deferred = 1;
    assert_eq!(
        round.observation().unwrap().1.outcome,
        RepairOutcome::Deferred(None)
    );
    round.application.as_mut().unwrap().deferred = 0;
    round.membership = Some(crate::control::Report {
        deferred: 1,
        ..Default::default()
    });
    assert_eq!(
        round.observation().unwrap().1.stage,
        Some(RepairStage::Membership)
    );
    round.membership = None;
    round.control.as_mut().unwrap().deferred = 1;
    assert_eq!(
        round.observation().unwrap().1.stage,
        Some(RepairStage::Control)
    );
    round.control.as_mut().unwrap().deferred = 0;
    round.handoff = Some(Err(failure(RepairCause::NotReady)));
    assert_eq!(
        round.observation().unwrap().1.stage,
        Some(RepairStage::Credentials)
    );
    assert_eq!(
        round.observation().unwrap().1.outcome,
        RepairOutcome::Failed(RepairCause::NotReady)
    );
    round.cancelled = true;
    assert_eq!(
        round.observation().unwrap().1.outcome,
        RepairOutcome::Cancelled
    );
}

#[test]
fn only_successful_outbound_rounds_expose_authenticated_endpoints() {
    use aterm_types::network_state::RepairCause;
    let address = "127.0.0.1:41384".parse().unwrap();
    let mut round = RepairRound::panicked(Some(address));
    round.actor = Some([7; 48]);
    round.exchange_started = true;
    round.phase = Some(crate::control::Phase::ControlComplete);
    round.control = Ok(crate::control::Report::default());
    round.completed_at_ms = 1000;
    let (actor, endpoint) = round.authenticated_endpoint().unwrap();
    assert_eq!(actor, [7; 48]);
    assert_eq!(endpoint.address, address);
    assert_eq!(endpoint.observed_at_ms, 1000);

    let failures: [fn(&mut RepairRound); 10] = [
        |r| r.target = None,
        |r| r.actor = None,
        |r| r.exchange_started = false,
        |r| r.cancelled = true,
        |r| r.control = Err(failure(RepairCause::Io)),
        |r| r.membership_error = Some(failure(RepairCause::Quota)),
        |r| r.application_error = Some(failure(RepairCause::Decode)),
        |r| r.application_interrupted = Some(crate::replica::Report::default()),
        |r| r.handoff = Some(Err(failure(RepairCause::NotReady))),
        |r| r.completed_at_ms = 0,
    ];
    for (index, fail) in failures.into_iter().enumerate() {
        let mut failed = round.clone();
        fail(&mut failed);
        assert!(
            failed.authenticated_endpoint().is_none(),
            "failure {index} promoted an endpoint"
        );
    }
}

/// Every way a round fails names its cause: a membership trade, a credential handoff, and a
/// control step after a finished application exchange (the shape of m22's 43 `failed (cause
/// unavailable)` records: stage application, sent and received 0; here a busy custody
/// refusing a re-check after the exchange), which the round's report once kept only as text.
/// The negative control is the type: a round failure is a [`Failure`], and an outcome
/// `RepairOutcome::Failed` without a cause does not compile (`aterm_types`' `compile_fail`
/// example).
#[test]
fn every_failed_round_names_its_cause() {
    use aterm_types::network_state::{RepairCause, RepairOutcome, RepairStage};
    let mut round = attempted();
    round.membership_error = Some(failure(RepairCause::Quota));
    let observation = round.observation().unwrap().1;
    assert_eq!(observation.stage, Some(RepairStage::Membership));
    assert_eq!(
        observation.outcome,
        RepairOutcome::Failed(RepairCause::Quota)
    );

    let mut round = attempted();
    round.handoff = Some(Err(failure(RepairCause::Io)));
    let observation = round.observation().unwrap().1;
    assert_eq!(observation.stage, Some(RepairStage::Credentials));
    assert_eq!(observation.outcome, RepairOutcome::Failed(RepairCause::Io));

    let mut round = attempted();
    round.phase = Some(crate::control::Phase::VersionReport);
    round.application = Some(crate::replica::Report {
        table: Some([3; 32]),
        ..Default::default()
    });
    round.control = Err(bounded_failure(&Failure::of(&crate::Error::busy())));
    let observation = round.observation().unwrap().1;
    assert_eq!(observation.stage, Some(RepairStage::Application));
    assert_eq!((observation.sent, observation.received), (Some(0), Some(0)));
    assert_eq!(
        observation.outcome,
        RepairOutcome::Failed(RepairCause::NotReady)
    );
}
