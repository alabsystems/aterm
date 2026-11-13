// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_containment::consent::{DebuggerProbeLabel, DevToolProbeLabel, FdaProbe, ProbeGate};

use super::*;
use crate::consent_state::ChangeKind;

fn slots(probe: FdaProbe) -> SlotFacts {
    SlotFacts {
        fda: (probe, Duration::ZERO),
        census: (None, false),
        debugger: DebuggerRow::refused(DebuggerProbeLabel::RefusedDisabled),
        devtools: DevToolsRow::refused(DevToolProbeLabel::RefusedDisabled, None),
    }
}

#[test]
fn privacy_read_observes_the_real_async_cache_completion() {
    let start = Instant::now();
    let mut state = ConsentState::inert();
    state.probes_for_test().live = true;
    let interval = Duration::from_secs(5);
    let seen = Cell::new(state.publication());
    let ((first, _), ticket) = state.read_at(ProbeGate::on(), interval, start);
    assert_eq!(first.label, ProbeLabel::Pending);
    let ticket = ticket.unwrap();
    let denied = consent::classify_probe(consent::ProbeOutcome::Errno(consent::ERRNO_EPERM));
    let rereads = Cell::new(0);
    let result = finish_privacy_read(
        slots(first),
        || {
            rereads.set(rereads.get() + 1);
            seen.set(state.publication());
            let ((current, _), next) = state.read_at(ProbeGate::on(), interval, start);
            assert!(
                next.is_none(),
                "reading a fresh completion does not launch a probe"
            );
            slots(current)
        },
        || start,
        |pending, remaining| {
            assert_eq!(pending.pending_kinds(), [ChangeKind::Fda]);
            assert_eq!(remaining, PRIVACY_READ_WAIT);
            state.complete_fda_for_test(ticket, denied, start);
            state.wait_for_change(
                seen.get(),
                &pending.pending_kinds(),
                Duration::from_secs(60),
            )
        },
    );
    assert!(!result.pending());
    assert_eq!(result.fda.0.state, FdaState::Denied);
    assert_eq!(rereads.get(), 1, "one publication, one reread");
}

#[test]
fn privacy_read_has_one_deadline_and_keeps_stuck_work_pending() {
    let start = Instant::now();
    let clock = Cell::new(start);
    let waits = Cell::new(0);
    let stuck = finish_privacy_read(
        slots(FdaProbe::pending()),
        || panic!("nothing published, so nothing is reread"),
        || clock.get(),
        |_, remaining| {
            waits.set(waits.get() + 1);
            assert_eq!(remaining, PRIVACY_READ_WAIT);
            clock.set(clock.get() + remaining);
            false
        },
    );
    assert!(stuck.pending());
    assert_eq!(stuck.fda.0.state, FdaState::Unknown);
    assert_eq!(clock.get() - start, PRIVACY_READ_WAIT);
    assert_eq!(waits.get(), 1);

    let clock = Cell::new(start);
    let rereads = Cell::new(0u32);
    let step = Duration::from_millis(200);
    let churned = finish_privacy_read(
        slots(FdaProbe::pending()),
        || {
            rereads.set(rereads.get() + 1);
            slots(FdaProbe::pending())
        },
        || clock.get(),
        |_, remaining| {
            assert_eq!(remaining, PRIVACY_READ_WAIT - step * rereads.get());
            let parked = step.min(remaining);
            clock.set(clock.get() + parked);
            parked == step
        },
    );
    assert!(churned.pending());
    assert_eq!(clock.get() - start, PRIVACY_READ_WAIT);
    assert_eq!(rereads.get(), 2, "one reread per publication");
}

#[test]
fn a_completed_privacy_read_neither_waits_nor_rereads() {
    let now = Instant::now();
    let completed = finish_privacy_read(
        slots(consent::classify_probe(consent::ProbeOutcome::Ok)),
        || panic!("a completed observation needs no reread"),
        || now,
        |_, _| panic!("a completed observation needs no wait"),
    );
    assert_eq!(completed.fda.0.state, FdaState::Granted);
}

#[test]
fn a_report_waits_on_exactly_its_pending_slots() {
    let answered = slots(consent::classify_probe(consent::ProbeOutcome::Ok));
    assert!(answered.pending_kinds().is_empty());
    assert!(!answered.pending());
    let mut pending = answered.clone();
    pending.census.1 = true;
    pending.devtools = DevToolsRow::refused(DevToolProbeLabel::Pending, None);
    assert_eq!(
        pending.pending_kinds(),
        [ChangeKind::Census, ChangeKind::Devtools]
    );
    pending.fda.0 = FdaProbe::pending();
    pending.debugger = DebuggerRow::refused(DebuggerProbeLabel::Pending);
    assert_eq!(
        pending.pending_kinds(),
        [
            ChangeKind::Fda,
            ChangeKind::Census,
            ChangeKind::Debugger,
            ChangeKind::Devtools
        ]
    );
}

fn app_data_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        AppDataFdaAuthority {
            const Buggy = 0;
            var grant = 0;
            var scope = 0;
            var adopted = 0;
            var decided = 0;
            var host_covered = 0;
            var session_covered = 0;
            action Grant when (decided == 0) { grant = 1; }
            action CurrentScope when (decided == 0) { scope = 1; }
            action FutureScope when (decided == 0) { scope = 2; }
            action Adopt when (decided == 0) { adopted = 1; }
            action Classify when (decided == 0) {
                host_covered = if (grant == 1 && scope > 0) || Buggy == 1 { 1 } else { 0 };
                session_covered = if Buggy == 1 && grant == 1 { 1 } else { 0 };
                decided = 1;
            }
            invariant HostNeedsAuthority: host_covered == 0 || (grant == 1 && scope > 0);
            invariant NoInheritedGrant: adopted == 0 || session_covered == 0;
            invariant Bounds: grant <= 1 && scope <= 2 && adopted <= 1 && decided <= 1 && host_covered <= 1 && session_covered <= 1;
        }
    }
}

#[test]
fn app_data_model_and_real_classifier_require_current_authority() {
    let model = app_data_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
        for scope in [
            consent::FdaScope::Unknown,
            consent::FdaScope::ThisProcess,
            consent::FdaScope::NewProcesses,
        ] {
            for adoption in [Attribution::Live, Attribution::Adopted] {
                let mut state = model.init_state();
                if fda == FdaState::Granted {
                    assert!(model.fire("Grant", &mut state));
                }
                match scope {
                    consent::FdaScope::Unknown => {}
                    consent::FdaScope::ThisProcess => {
                        assert!(model.fire("CurrentScope", &mut state))
                    }
                    consent::FdaScope::NewProcesses => {
                        assert!(model.fire("FutureScope", &mut state))
                    }
                }
                if adoption == Attribution::Adopted {
                    assert!(model.fire("Adopt", &mut state));
                }
                let expected = model.successors("Classify", &state);
                let mut projected = expected[0].clone();
                projected.insert("host_covered", i64::from(app_data_covered(fda, scope)));
                let posture = ConsentPosture::join(PostureInputs {
                    adoption,
                    fda,
                    responsible: Responsible::Unknown,
                    observed_eperm: false,
                    evidence: SpikeEvidence::UNMEASURED,
                });
                projected.insert(
                    "session_covered",
                    i64::from(posture.fs_consent == FsConsent::Covered),
                );
                assert!(expected.contains(&projected));
                if adoption == Attribution::Adopted {
                    projected.insert("session_covered", 1);
                    assert!(!model.check_invariant("NoInheritedGrant", &projected));
                }
            }
        }
        let split = covers_split(fda, SpikeEvidence::UNMEASURED);
        assert_eq!(split.covers.contains(&"app-data"), fda == FdaState::Granted);
        assert_eq!(
            split.unmeasured.contains(&"app-data"),
            fda != FdaState::Granted
        );
        assert_eq!(
            observed_fda_scope(fda, consent::FdaScope::Unknown),
            if fda == FdaState::Granted {
                consent::FdaScope::ThisProcess
            } else {
                consent::FdaScope::Unknown
            }
        );
    }
}
