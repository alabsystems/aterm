// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn cancel_case(state: &aterm_spec::interp::State, buggy: u8) -> aterm_spec::interp::State {
    let (_tmp, env, mut stored, _) = fixture();
    stored.launch_attempted = state["stored"] == 1;
    stored.phase = if stored.launch_attempted {
        Phase::Committing
    } else {
        Phase::Ready
    };
    let mut expected = stored.clone();
    expected.launch_attempted = state["caller"] == 1;
    expected.phase = if expected.launch_attempted {
        Phase::Committing
    } else {
        Phase::Ready
    };
    let fault = state["fault"];
    let acknowledged = with_receipt_writer(&env, |writer| {
        write_receipt(&env, &stored)?;
        let failure = (fault > 0).then(|| {
            aterm_durable::seam::FailingDirs::after(
                u32::try_from(fault - 1).unwrap(),
                aterm_durable::seam::EIO,
            )
        });
        let result = writer.cancel(&expected, CancellationKind::OwnerStop, 5, 5);
        drop(failure);
        if matches!(buggy, 1 | 2) && matches!(fault, 0 | 4) {
            // Negative controls: reproduce the old stored-only projection,
            // or replace it with a caller-only projection that loses disk history.
            let mut wrong = read_root(&env, &stored.id)?.unwrap();
            wrong.launch_attempted = if buggy == 1 {
                stored.launch_attempted
            } else {
                expected.launch_attempted
            };
            write_receipt(&env, &wrong)?;
        }
        Ok(if buggy == 3 {
            true
        } else if buggy == 4 {
            false
        } else {
            result.is_ok()
        })
    })
    .unwrap()
    .unwrap();
    let raw = read_root(&env, &stored.id).unwrap().unwrap();
    let mut next = state.clone();
    next.insert("done", 1);
    next.insert("marker", i64::from(raw.launch_attempted));
    next.insert("acknowledged", i64::from(acknowledged));
    next
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "StoredAttempt" => {
            next.insert("stored", 1);
            next.insert("marker", 1);
        }
        "CallerAttempt" => {
            next.insert("caller", 1);
        }
        "NextFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Cancel" => return cancel_case(state, 0),
        _ => unreachable!(),
    }
    next
}

#[test]
fn cancellation_attempt_history_refines_real_files_at_every_directory_barrier() {
    let model = aterm_models_harness::cancellation_history_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (buggy, stored, caller, fault, law) in [
        (1, 0, 1, 0, "AcknowledgedHistory"),
        (1, 0, 1, 4, "VisibleHistory"),
        (2, 1, 0, 0, "AcknowledgedHistory"),
        (2, 1, 0, 4, "VisibleHistory"),
        (3, 0, 1, 1, "DurableReturn"),
        (4, 0, 1, 0, "Available"),
    ] {
        let mut state = model.init_state();
        state.insert("stored", stored);
        state.insert("marker", stored);
        state.insert("caller", caller);
        state.insert("fault", fault);
        let next = cancel_case(&state, buggy);
        assert!(!model.check_invariant(law, &next), "{law}");
        assert!(!model.successors("Cancel", &state).contains(&next));
    }
}

#[test]
fn cancellation_attempt_history_repairs_each_failed_barrier() {
    for failed_barrier in 0..4 {
        let (_tmp, env, mut stored, _) = fixture();
        stored.phase = Phase::Ready;
        stored.launch_attempted = false;
        let mut expected = stored.clone();
        expected.phase = Phase::Committing;
        expected.launch_attempted = true;
        with_receipt_writer(&env, |writer| {
            write_receipt(&env, &stored)?;
            let failure =
                aterm_durable::seam::FailingDirs::after(failed_barrier, aterm_durable::seam::EIO);
            assert!(
                writer
                    .cancel(&expected, CancellationKind::OwnerStop, 5, 5)
                    .is_err()
            );
            drop(failure);
            let first = writer.cancel(&expected, CancellationKind::NewPersonTask, 6, 6)?;
            let retired = read_root(&env, &stored.id)?.unwrap();
            assert_eq!(retired.phase, Phase::Retired);
            assert!(retired.revoked && retired.launch_attempted);
            assert_eq!(read_receipt(&env, &stored.id), Some(retired.clone()));
            assert_eq!(
                writer.cancel(&expected, CancellationKind::PolicyRevoked, 7, 7)?,
                first
            );
            assert_eq!(read_root(&env, &stored.id)?, Some(retired));
            Ok(())
        })
        .unwrap()
        .unwrap();
    }
}
