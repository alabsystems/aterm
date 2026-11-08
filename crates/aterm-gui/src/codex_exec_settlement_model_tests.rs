// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_spec::interp::State as Projection;

fn poll_case(before: &Projection, mutant: u8) -> Projection {
    let case = before["case"];
    let fixture = Fixture::new(case > 0, case >= 2);
    if case >= 3 {
        let consumed = fixture.consumed();
        if case == 4 {
            let mut state = fixture.execution.lock();
            assert!(state.tx.load(2));
            assert!(state.tx.consume(2));
            state.receipt = consumed;
            state.custody.take();
        }
    }
    let now = Instant::now();
    if before["pending"] == 1 {
        // Reproduce the prior successful or failed real poll. A recovery event
        // changes the next poll's storage, not its predecessor's outcome.
        let failure = (before["ack"] == 0)
            .then(|| aterm_durable::seam::FailingDirs::with(aterm_durable::seam::EIO));
        fixture.poll(before["custody"] == 0, now);
        drop(failure);
    }
    let failure = (before["failed"] == 1)
        .then(|| aterm_durable::seam::FailingDirs::with(aterm_durable::seam::EIO));
    let result = fixture.poll(before["gone"] == 1, now + Duration::from_secs(31));
    drop(failure);
    let mut state = fixture.execution.lock();
    let mut next = before.clone();
    next.insert("polled", 1);
    next.insert("cancelled", i64::from(state.tx.stage() == Stage::Cancelled));
    next.insert("pending", i64::from(state.pending.is_some()));
    next.insert(
        "ack",
        i64::from(state.pending.as_ref().is_some_and(|p| p.acknowledged)),
    );
    next.insert("custody", i64::from(state.custody.is_some()));
    next.insert(
        "native_consumed",
        i64::from(matches!(state.tx.stage(), Stage::Consumed(_))),
    );
    next.insert("marker", i64::from(state.receipt.launch_attempted));
    next.insert(
        "late",
        i64::from(state.tx.stage() == Stage::Cancelled && state.tx.execute(1)),
    );
    next.insert("terminate", i64::from(result.terminate));
    next.insert("done", i64::from(result.done));
    // Observable negative controls for the historical failure modes. Each must
    // leave both the implementation conformance relation and a named law.
    match mutant {
        1 => {
            next.insert("ack", 1);
        }
        2 => {
            next.insert("done", 1);
        }
        3 => {
            next.insert("cancelled", 0);
        }
        4 => {
            next.insert("marker", 0);
        }
        5 => {
            next.insert("native_consumed", 1);
        }
        6 => {
            next.insert("terminate", 1);
        }
        7 => {
            next.insert("late", 1);
        }
        8 => {
            next.insert("pending", 0);
        }
        9 => {
            next.insert("ack", 0);
        }
        10 => {
            next.insert("custody", 0);
        }
        _ => {}
    }
    next
}

pub(crate) fn transition(before: &Projection, action: &str) -> Projection {
    if action == "Poll" {
        return poll_case(before, 0);
    }
    let mut next = before.clone();
    match action {
        "NextCase" => {
            next.insert("case", before["case"] + 1);
            next.insert("marker", 1);
            next.insert("native_consumed", i64::from(before["case"] == 3));
            next.insert("custody", i64::from(before["case"] != 3));
        }
        "Fail" => {
            next.insert("failed", 1);
            next.insert("polled", 0);
        }
        "Recover" => {
            next.insert("failed", 0);
            next.insert("polled", 0);
        }
        "SourceGone" => {
            next.insert("gone", 1);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn settlement_real_polls_refine_owner_loss_and_storage_recovery() {
    let model = aterm_models_harness::exec_settlement_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for before in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &before) {
                let after = transition(&before, action.name);
                assert!(
                    model.successors(action.name, &before).contains(&after),
                    "{}: {before:?} -> {after:?}",
                    action.name
                );
            }
        }
    }
    for (mutant, case, failed, gone, law) in [
        (1, 2, 1, 1, "DurableAck"),
        (2, 2, 1, 1, "NoPrematureFinish"),
        (3, 2, 1, 0, "AdmissionClosed"),
        (4, 1, 0, 1, "AttemptHistory"),
        (5, 3, 0, 1, "NoPhantomConsumption"),
        (6, 1, 0, 0, "TerminationNeedsGo"),
        (7, 2, 0, 0, "NoLateAdmission"),
        (8, 2, 1, 0, "PendingRetained"),
        (9, 2, 0, 0, "Available"),
        (10, 1, 0, 0, "CustodyUntilClosure"),
    ] {
        let mut before = model.init_state();
        before.insert("case", case);
        before.insert("marker", i64::from(case > 0));
        before.insert("failed", failed);
        before.insert("gone", gone);
        let after = poll_case(&before, mutant);
        assert!(!model.check_invariant(law, &after), "{law}");
        assert!(!model.successors("Poll", &before).contains(&after));
    }
}
