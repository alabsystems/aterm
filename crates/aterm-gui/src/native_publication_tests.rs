// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::native_receipts::testing::{Append, Journal, State as Stored};

fn stored_number(state: Stored) -> i64 {
    match state {
        Stored::Prepared => 0,
        Stored::MayExecute => 1,
        Stored::NoExec => 2,
        Stored::Sealed { may_execute: false } => 3,
        Stored::Sealed { may_execute: true } => 4,
    }
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_by(state, action, false, false)
}

fn transition_by(
    state: &aterm_spec::interp::State,
    action: &str,
    reply_selects_record: bool,
    detached_custody: bool,
) -> aterm_spec::interp::State {
    let home = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::new(home.path());
    let (handle, facts, prepare, actor) = closed_restart::fixture(8112);
    let ctx = &handle.ctx;
    let binding = admitted_binding(&run(ctx, &facts, Request::prepare(prepare.clone()), actor));
    let operation = lookup(ctx, &binding).unwrap();
    journal.record(&prepare, &binding, Append::Prepare).unwrap();
    if state["may"] == 1 {
        journal
            .record(&prepare, &binding, Append::MayExecute)
            .unwrap();
        operation.lock().unwrap().grant_record = GrantRecord::Recorded;
    }
    match state["journal"] {
        2 => {
            journal.record(&prepare, &binding, Append::NoExec).unwrap();
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
                completed_reply(&operation)
            } else {
                UNKNOWN.into()
            };
            next.insert("reply", i64::from(reply == NOEXEC));
            next.insert("captured", 1);
        }
        "PublishAck" | "PublishLostAck" | "PublishFailed" => {
            let reply = if state["reply"] == 1 { NOEXEC } else { UNKNOWN };
            let mut record = terminal_record(&operation.lock().unwrap(), reply);
            if reply_selects_record && record.is_some() {
                // The former bug: Unknown text hid a settled NoExec fact,
                // permanently closing its real journal entry as an opaque seal.
                record = Some(if reply == NOEXEC {
                    TerminalRecord::NoExec
                } else {
                    TerminalRecord::Sealed
                });
            }
            let output = publish_terminal(reply.into(), record, |record| {
                if action == "PublishFailed" {
                    return false;
                }
                let event = match record {
                    TerminalRecord::NoExec => Append::NoExec,
                    TerminalRecord::Sealed => Append::Seal,
                };
                let committed = journal.record(&prepare, &binding, event).is_ok();
                committed && action != "PublishLostAck"
            });
            if output == NOEXEC {
                next.insert("published", 1);
            }
            next.insert("captured", 0);
        }
        "Observe" => {
            if journal.observe(&binding).unwrap().is_some() {
                next.insert("observed", 1);
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
    if reply_selects_record && next["journal"] == 3 {
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
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                for detached_custody in [false, true] {
                    let next = if detached_custody {
                        transition_by(&state, action.name, false, true)
                    } else {
                        transition(&state, action.name)
                    };
                    assert!(
                        model.successors(action.name, &state).contains(&next),
                        "{} (detached {detached_custody}): {state:?} -> {next:?}",
                        action.name
                    );
                }
            }
        }
    }

    let mut settled = model.init_state();
    settled.insert("kind", 1);
    settled.insert("drained", 1);
    settled.insert("captured", 1);
    assert!(aterm_spec::interp::reachable_states(&model).contains(&settled));
    let bad = transition_by(&settled, "PublishAck", true, false);
    assert_eq!(
        bad["journal"], 3,
        "negative control must really seal the journal"
    );
    assert!(!model.check_invariant("NoExecRemainsRecoverable", &bad));
    // Real storage refuses to change that opaque seal into a NoExec receipt.
    // This is precisely the liveness loss a permissive test map would hide.
}
