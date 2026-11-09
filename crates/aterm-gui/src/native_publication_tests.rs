// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::native_receipts::testing::{Append, Journal, State as Stored};
use aterm_spec::xref::StepEvidence;

fn witness<T>(
    evidence: &mut Option<&mut StepEvidence>,
    action: &'static str,
    drive: impl FnOnce() -> T,
) -> T {
    match evidence.as_deref_mut() {
        Some(evidence) => evidence.step(action, drive),
        None => drive(),
    }
}

fn stored_number(state: Stored) -> i64 {
    match state {
        Stored::Prepared => 0,
        Stored::MayExecute => 1,
        Stored::NoExec => 2,
        Stored::Sealed { may_execute: false } => 3,
        Stored::Sealed { may_execute: true } => 4,
        Stored::Fenced => 5,
        Stored::Rejected => panic!("a rejected recipe is not a published admission"),
    }
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_by(state, action, 0, false, None)
}

fn transition_by(
    state: &aterm_spec::interp::State,
    action: &str,
    fault: u8,
    detached_custody: bool,
    mut evidence: Option<&mut StepEvidence>,
) -> aterm_spec::interp::State {
    let home = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::new(home.path());
    let (handle, facts, prepare, actor) = closed_restart::fixture(8112);
    let ctx = &handle.ctx;
    let binding = admitted_binding(&run(ctx, &facts, Request::prepare(prepare.clone()), actor));
    let operation = lookup(ctx, &binding).unwrap();
    operation.lock().unwrap().resume_phase = (state["active"] == 1).then_some(ImagePhase::Rollback);
    journal.record(&prepare, &binding, Append::Prepare).unwrap();
    if state["may"] == 1 {
        journal
            .record(&prepare, &binding, Append::MayExecute)
            .unwrap();
        operation.lock().unwrap().grant_record = GrantRecord::Recorded;
    }
    match state["journal"] {
        2 => {
            let phase = if state["journal_phase"] == 1 {
                ImagePhase::Rollback
            } else {
                ImagePhase::Forward
            };
            journal
                .record(&prepare, &binding, Append::NoExecInPhase(phase))
                .unwrap();
        }
        3 | 4 => {
            journal.record(&prepare, &binding, Append::Seal).unwrap();
        }
        _ => {}
    }
    assert_eq!(
        stored_number(journal.state(&binding).unwrap().unwrap()),
        state["journal"]
    );

    // An actual pending producer retains NativeCustody. Dropping it, rather
    // than setting a boolean mock, is the model's external Drain action.
    let custody = operation.lock().unwrap().custody.as_ref().unwrap().clone();
    let snapshot = ctx
        .meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us());
    let input = Arc::new(NativeInput::new(
        ctx,
        custody,
        ctx.human_input.seq(),
        snapshot,
        facts.clock.load(Ordering::SeqCst) + MAX_PREPARE_US,
        Arc::new(AtomicBool::new(false)),
        Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
        Clock::Facts(Arc::clone(&facts.clock)),
    ));
    let mut pending = Some(input.reserve().unwrap());
    input.seal();
    if detached_custody {
        // An uncertain preparation releases its own custody, while another
        // admitted producer can still retain the exact native marker.
        let custody = {
            let mut op = operation.lock().unwrap();
            op.completion_drain = Some(op.custody.as_ref().unwrap().drain());
            op.custody.take()
        };
        drop(custody);
    }
    let close = |kind| {
        let decision = if kind == 1 {
            Decision::Retired(Retirement::NotGranted)
        } else {
            Decision::Unknown
        };
        let completion = operation.lock().unwrap().seal_completion(decision);
        assert_eq!(finish(ctx, &operation, completion), UNKNOWN);
    };
    if state["kind"] > 0 {
        close(state["kind"]);
    }
    if state["drained"] == 1 {
        drop(pending.take());
    }

    let mut next = state.clone();
    match action {
        "StartRollback" => {
            operation.lock().unwrap().resume_phase = Some(ImagePhase::Rollback);
            next.insert("active", 1);
        }
        "MayExecute" => {
            journal
                .record(&prepare, &binding, Append::MayExecute)
                .unwrap();
            operation.lock().unwrap().grant_record = GrantRecord::Recorded;
            next.insert("may", 1);
        }
        "CloseNoExec" | "CloseUnknown" => {
            let kind = if action == "CloseNoExec" { 1 } else { 2 };
            close(kind);
            next.insert("kind", kind);
        }
        "Drain" => {
            drop(pending.take());
        }
        "Capture" | "CaptureUnknown" => {
            let reply = if action == "Capture" {
                witness(&mut evidence, "Capture", || completed_reply(&operation))
            } else {
                UNKNOWN.into()
            };
            next.insert("reply", i64::from(reply == NOEXEC));
            next.insert("captured", 1);
        }
        "PublishAck" | "PublishLostAck" | "PublishFailed" => {
            let reply = if state["reply"] == 1 { NOEXEC } else { UNKNOWN };
            let mut record = terminal_record(&operation.lock().unwrap(), reply);
            if fault == 1 && record.is_some() {
                // The former bug: Unknown text hid a settled NoExec fact,
                // permanently closing its real journal entry as an opaque seal.
                record = Some(if reply == NOEXEC {
                    TerminalRecord::NoExec(ImagePhase::Forward)
                } else {
                    TerminalRecord::Sealed
                });
            }
            if fault == 6 && matches!(record, Some(TerminalRecord::NoExec(_))) {
                record = Some(TerminalRecord::NoExec(ImagePhase::Forward));
            }
            let publication = match action {
                "PublishAck" => "PublishAck",
                "PublishLostAck" => "PublishLostAck",
                "PublishFailed" => "PublishFailed",
                _ => unreachable!(),
            };
            let output = witness(&mut evidence, publication, || {
                publish_terminal(reply.into(), record, |record| {
                    if action == "PublishFailed" {
                        return false;
                    }
                    let event = match record {
                        TerminalRecord::NoExec(phase) => Append::NoExecInPhase(phase),
                        TerminalRecord::Sealed => Append::Seal,
                    };
                    let committed = journal.record(&prepare, &binding, event).is_ok();
                    committed && action != "PublishLostAck"
                })
            });
            if output == NOEXEC {
                next.insert("published", 1);
            }
            next.insert("captured", 0);
        }
        "Observe" => {
            let observation = witness(&mut evidence, "Observe", || {
                journal.observe_phase(&prepare, &binding).unwrap()
            });
            if let Some(observation) = observation {
                next.insert("observed", 1);
                let phase = i64::from(observation.phase == ImagePhase::Rollback);
                next.insert("observed_phase", if fault == 7 { 1 - phase } else { phase });
            }
        }
        _ => panic!("unknown terminal publication action {action}"),
    }
    let op = operation.lock().unwrap();
    next.insert(
        "kind",
        match op.phase {
            Phase::Completed(Decision::Retired(Retirement::NotGranted)) => 1,
            Phase::Completed(Decision::Unknown) => 2,
            Phase::Started => 0,
            _ => panic!("unexpected publication phase {:?}", op.phase),
        },
    );
    next.insert(
        "drained",
        i64::from(
            op.completion_drain
                .as_ref()
                .is_some_and(NativeDrain::is_drained),
        ),
    );
    drop(op);
    // state() reopens the committed astream data, not an in-memory test map.
    next.insert(
        "journal",
        stored_number(journal.state(&binding).unwrap().unwrap()),
    );
    next.insert(
        "journal_phase",
        journal
            .observe_phase(&prepare, &binding)
            .unwrap()
            .map_or(2, |observation| {
                i64::from(observation.phase == ImagePhase::Rollback)
            }),
    );
    if fault == 1 && next["journal"] == 3 {
        assert!(
            journal.record(&prepare, &binding, Append::NoExec).is_err(),
            "an opaque terminal seal cannot be upgraded by a later retry"
        );
    }
    next
}

#[test]
fn terminal_publication_refines_host_drain_and_real_astream_journal() {
    let model = aterm_models_harness::terminal_publication_model();
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut evidence = StepEvidence::new("TerminalPublication");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                for detached_custody in [false, true] {
                    let next = transition_by(
                        &state,
                        action.name,
                        0,
                        detached_custody,
                        Some(&mut evidence),
                    );
                    assert!(
                        model.successors(action.name, &state).contains(&next),
                        "{} (detached {detached_custody}): {state:?} -> {next:?}",
                        action.name
                    );
                }
            }
        }
    }
    let audit = evidence.audit();
    assert!(audit.is_complete(), "{audit:?}");

    let mut settled = model.init_state();
    settled.insert("kind", 1);
    settled.insert("drained", 1);
    settled.insert("captured", 1);
    assert!(aterm_spec::interp::reachable_states(&model).contains(&settled));
    let bad = transition_by(&settled, "PublishAck", 1, false, None);
    assert_eq!(
        bad["journal"], 3,
        "negative control must really seal the journal"
    );
    assert!(!model.check_invariant("NoExecRemainsRecoverable", &bad));
    // Real storage refuses to change that opaque seal into a NoExec receipt.
    // This is precisely the liveness loss a permissive test map would hide.
}

#[test]
fn dropping_or_misreporting_the_closed_phase_breaks_production_conformance() {
    let model = aterm_models_harness::terminal_publication_model();
    let mut state = model.init_state();
    for action in ["StartRollback", "CloseNoExec", "Drain", "Capture"] {
        state = transition(&state, action);
    }
    assert!(aterm_spec::interp::reachable_states(&model).contains(&state));
    let wrong_write = transition_by(&state, "PublishAck", 6, false, None);
    assert_eq!(wrong_write["journal"], 2);
    assert_eq!(wrong_write["journal_phase"], 0);
    assert!(!model.check_invariant("JournalCarriesClosedPhase", &wrong_write));
    assert!(
        !model
            .successors("PublishAck", &state)
            .contains(&wrong_write)
    );

    let committed = transition(&state, "PublishAck");
    let wrong_read = transition_by(&committed, "Observe", 7, false, None);
    assert!(!model.check_invariant("ObservationCarriesClosedPhase", &wrong_read));
    assert!(
        !model
            .successors("Observe", &committed)
            .contains(&wrong_read)
    );
}
