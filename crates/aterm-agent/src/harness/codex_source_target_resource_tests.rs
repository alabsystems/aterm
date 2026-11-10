// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::{
    account_work, codex_session_slot::metadata_fixture, codex_source_registry::FixtureConnection,
};

fn wire(report: &Report) -> ResourceReport<'_> {
    ResourceReport {
        operation: &report.operation,
        member: &report.member,
        registration: &report.registration,
        boundary: &report.boundary,
        assignment_generation: report.assignment_generation,
        credential_generation: report.credential_generation,
        digest: &report.digest,
        account: &report.account,
        user: &report.user,
        auth: report.auth,
    }
}
struct Fixture {
    registry: Registry,
    lifetime: account_work::Lifetime,
    caller: slot::Caller,
    process: Process,
    connection: FixtureConnection,
    source: Arc<slot::SourceHandle>,
    report: Report,
    work: Option<Arc<Work>>,
}
impl Fixture {
    fn new(pending: bool) -> Self {
        // Metadata-ledger injection only; no InstalledTarget, owner, signed
        // Book, supported image, resource-ready admission or ACK is minted.
        // Process observation, Scope counting/cancellation and EOF are real.
        let lifetime = account_work::Lifetime::default();
        let registry = Registry::for_app(&lifetime);
        let process = Process::observe(std::process::id()).unwrap();
        let caller = slot::Caller {
            sid: "s-resource-ledger".into(),
            pid: process.pid,
            birth: process.birth,
        };
        let connection = registry.fixture_connection();
        let source = Arc::new(metadata_fixture::with_assignment(
            metadata_fixture::source(
                metadata_fixture::binding("fixture"),
                std::path::PathBuf::from("/synthetic/not-read"),
                13,
            ),
            8,
            &"c".repeat(32),
        ));
        let report = Report {
            operation: "a".repeat(32),
            member: "b".repeat(32),
            registration: "c".repeat(32),
            boundary: "d".repeat(32),
            assignment_generation: 8,
            credential_generation: 13,
            digest: source.digest().into(),
            account: source.account().into(),
            user: source.user().into(),
            auth: AuthState {
                generation: 17,
                owner: 19,
                source: 23,
                login: 29,
            },
        };
        let work = Arc::new(Work {
            scope: lifetime.enter().unwrap(),
            worker: false.into(),
        });
        registry.state.lock().unwrap().target_resources.insert(
            open_key(&caller, &report.operation),
            Record {
                caller: caller.clone(),
                process: process.clone(),
                assignment: "c".repeat(32),
                context: "e".repeat(64),
                report: report.clone(),
                source: source.clone(),
                connection: connection.clone(),
                pending: pending.then(|| work.clone()),
                work_identity: Arc::downgrade(&work),
                receipt: (!pending).then(|| "f".repeat(64)),
                cohort: Vec::new(),
                ack: None,
                ack_retirement: None,
                applied: None,
                normalized: None,
                refused: false,
                revoked: false,
                cancellation: work.scope.cancellation(),
            },
        );
        Self {
            registry,
            lifetime,
            caller,
            process,
            connection,
            source,
            report,
            work: pending.then_some(work),
        }
    }
    fn replay(&self, report: &Report) -> Result<Option<TargetResourceReceipt>, Error> {
        self.registry.replay_target_resources(
            &self.caller,
            &"c".repeat(32),
            &"e".repeat(64),
            wire(report),
        )
    }
}

#[test]
fn issued_resource_correlation_survives_eof_but_explicit_retirement_revokes_it() {
    let f = Fixture::new(false);
    assert!(f.replay(&f.report).unwrap().is_some());
    f.registry.connection_closed(f.connection.id);
    assert!(f.replay(&f.report).unwrap().is_some());
    assert!(
        f.registry
            .replay_target_resources(&f.caller, &"0".repeat(32), &"e".repeat(64), wire(&f.report))
            .is_err()
    );
    assert!(
        f.registry
            .replay_target_resources(&f.caller, &"c".repeat(32), &"0".repeat(64), wire(&f.report))
            .is_err()
    );
    let mut changed = f.report.clone();
    changed.registration = "0".repeat(32);
    assert!(f.replay(&changed).is_err());
    let retired = revoke_member(
        &mut f.registry.state.lock().unwrap(),
        &f.caller,
        &f.report.member,
    );
    drop(retired);
    assert!(f.replay(&f.report).is_err());
    let mut unknown = f.report.clone();
    unknown.operation = "1".repeat(32);
    assert!(f.replay(&unknown).unwrap().is_none());
    unknown.assignment_generation = 0;
    assert!(f.replay(&unknown).is_err());
}

#[test]
fn cancelled_unstarted_resource_fences_late_arm_and_keeps_captures_counted() {
    let mut f = Fixture::new(true);
    let command_capture = f.work.take().unwrap();
    let pause = f.lifetime.pause().unwrap();
    assert_eq!(f.registry.revoke_cancelled_unclaimed().unwrap(), 1);
    let pause = pause
        .try_wait()
        .expect_err("removed Pending is not last-use proof");
    // Production run invokes this exact fence before even physical probes.
    assert!(
        arm_work(
            &f.registry.state.lock().unwrap(),
            &f.caller,
            &f.report,
            &f.process,
            &f.source,
            &command_capture
        )
        .is_err()
    );
    drop(command_capture);
    assert!(pause.try_wait().is_ok());
}

#[test]
fn failed_dispatch_requires_exact_work_identity_and_does_not_release_captured_scope() {
    let mut f = Fixture::new(true);
    let command_capture = f.work.take().unwrap();
    let wrong = Arc::new(Work {
        scope: f.lifetime.enter().unwrap(),
        worker: false.into(),
    });
    assert!(
        refuse_work(
            &mut f.registry.state.lock().unwrap(),
            &f.caller,
            &f.report,
            &f.process,
            &wrong
        )
        .is_err()
    );
    drop(wrong);
    let retired = refuse_work(
        &mut f.registry.state.lock().unwrap(),
        &f.caller,
        &f.report,
        &f.process,
        &command_capture,
    )
    .unwrap();
    drop(retired);
    let pause = f.lifetime.pause().unwrap();
    let pause = pause
        .try_wait()
        .expect_err("captured work is still counted");
    assert!(
        arm_work(
            &f.registry.state.lock().unwrap(),
            &f.caller,
            &f.report,
            &f.process,
            &f.source,
            &command_capture
        )
        .is_err()
    );
    drop(command_capture);
    assert!(pause.try_wait().is_ok());
}
#[test]
fn resource_cohort_survives_eof_but_registration_or_actual_readiness_replacement_refuses() {
    let f = Fixture::new(false);
    let root = aterm_tempfile::tempdir().unwrap();
    let gate = SourceWorkLease::for_home(root.path()).unwrap();
    let mut readiness = SourceReadiness::preload(gate, None).unwrap();
    readiness
        .complete(ReadyRole::Manager(f.report.auth))
        .unwrap();
    let key = member_key(&f.caller, &f.report.member).unwrap();
    let mut state = f.registry.state.lock().unwrap();
    state.members.insert(
        key.clone(),
        Member {
            caller: f.caller.clone(),
            process: f.process.clone(),
            operation: f.report.operation.clone(),
            registration: f.report.registration.clone(),
            connection: f.connection.clone(),
            version: 1,
            source: f.source.clone(),
            readiness,
            unknown: false,
            work_rebound: false,
        },
    );
    let captured = vec![CohortMember::Loaded {
        member: f.report.member.clone(),
        registration: f.report.registration.clone(),
        source: f.source.clone(),
        role: ReadyRole::Manager(f.report.auth),
    }];
    assert!(cohort_current(&state, &f.caller, &captured));
    state.members.get_mut(&key).unwrap().unknown = true;
    f.connection.alive.store(false, Ordering::Release);
    assert!(
        cohort_current(&state, &f.caller, &captured),
        "EOF alone does not rewrite an issued observation"
    );
    state.members.get_mut(&key).unwrap().registration = "0".repeat(32);
    assert!(!cohort_current(&state, &f.caller, &captured));
    state.members.get_mut(&key).unwrap().registration = f.report.registration.clone();
    state.members.get_mut(&key).unwrap().readiness.role = Some(ReadyRole::PolicyUnavailable);
    assert!(!cohort_current(&state, &f.caller, &captured));
    state.members.get_mut(&key).unwrap().readiness.role = Some(ReadyRole::Manager(AuthState {
        generation: f.report.auth.generation + 1,
        ..f.report.auth
    }));
    assert!(!cohort_current(&state, &f.caller, &captured));
}

#[test]
fn registered_resource_frontdoor_replays_only_native_issued_exact_observation_without_new_work() {
    // Injected record only; real process, EOF and App-drain state. No installed
    // target/source or all-participant readiness is constructed by this fixture.
    let f = Fixture::new(false);
    let resource = || {
        f.registry.prepare_registered_target_resources(
            &f.caller,
            &f.connection,
            &"c".repeat(32),
            &"e".repeat(64),
            wire(&f.report),
        )
    };
    assert!(matches!(resource(), Ok(ResourcePreparation::Ready(_))));
    f.registry.connection_closed(f.connection.id);
    let _drained = f.lifetime.pause().unwrap().wait();
    assert!(
        matches!(resource(), Ok(ResourcePreparation::Ready(_))),
        "issued-receipt replay is metadata after EOF/drain"
    );
    assert!(
        f.registry
            .prepare_registered_target_resources(
                &f.caller,
                &f.connection,
                &"c".repeat(32),
                &"0".repeat(64),
                wire(&f.report)
            )
            .is_err()
    );
    let mut changed = f.report.clone();
    changed.auth.source += 1;
    assert!(
        f.registry
            .prepare_registered_target_resources(
                &f.caller,
                &f.connection,
                &"c".repeat(32),
                &"e".repeat(64),
                wire(&changed)
            )
            .is_err()
    );
    let retired = revoke_member(
        &mut f.registry.state.lock().unwrap(),
        &f.caller,
        &f.report.member,
    );
    drop(retired);
    assert!(
        resource().is_err(),
        "explicit native retirement revokes resource admission"
    );
    assert!(
        f.registry
            .prepare_registered_target_ack(
                &f.caller,
                &"c".repeat(32),
                &"e".repeat(64),
                &"f".repeat(64)
            )
            .is_err(),
        "a resource token without actual installed role/ACK observation cannot mint Applied"
    );
}
