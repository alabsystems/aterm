// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn consumed_fixture(revoked: bool) -> (aterm_tempfile::TempDir, Env, Receipt) {
    let (tmp, env, mut root, _) = fixture();
    root.phase = Phase::Consumed;
    root.accepted_turn = Some(root.intent.wall.clone());
    root.successor_pid = Some(40);
    root.successor_birth = Some(50);
    root.current_attempt = Some(AttemptRef {
        id: "ef".repeat(16),
        generation: 1,
    });
    root.candidate = Some(Candidate {
        pid: 40,
        birth: 50,
        parent: 20,
        group: 40,
        session: 20,
        launcher_device: 1,
        launcher_inode: 2,
        launcher_path: b"/fixture/codex".to_vec(),
        forward: true,
    });
    root.revoked = revoked;
    with_receipt_writer(&env, |_| write_receipt(&env, &root))
        .unwrap()
        .unwrap();
    (tmp, env, root)
}

fn proposal(root: &Receipt, kind: i64) -> Receipt {
    let mut next = root.clone();
    match kind {
        0 | 8 => {}
        1 => next.phase = Phase::Retired,
        2 => next.accepted_turn = None,
        3 => next.generation += 1,
        4 => next.launch_attempted = false,
        5 => next.revoked = false,
        6 => {
            next.successor_pid = Some(60);
            next.successor_birth = Some(70);
        }
        7 => next.revoked = true,
        9 => next.current_attempt = None,
        10 => next.candidate = None,
        11 => next.successor_pid = None,
        12 => next.successor_birth = None,
        _ => unreachable!(),
    }
    next
}

fn publish_case(state: &aterm_spec::interp::State, buggy: u8) -> aterm_spec::interp::State {
    let kind = state["kind"];
    let fault = state["fault"];
    let (_tmp, env, root) = consumed_fixture(state["revoked"] == 1);
    let next = proposal(&root, kind);
    let path = receipt_path(&env, &root.id).unwrap();
    if kind == 8 {
        // Deliberately corrupt a retained file, bypassing production publication.
        std::fs::write(&path, b"incomplete receipt").unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let (result, barrier) = with_receipt_writer(&env, |_| {
        let failure = match fault {
            1 => Some(aterm_durable::seam::FailingDirs::with(
                aterm_durable::seam::EIO,
            )),
            2 => Some(aterm_durable::seam::FailingDirs::after(
                1,
                aterm_durable::seam::EIO,
            )),
            _ => None,
        };
        let recording = aterm_durable::seam::Recording::start();
        let mut result = if buggy == 3 && kind == 0 {
            // Negative control: equal bytes return early, without durability repair.
            Ok(())
        } else if buggy == 6 && matches!(kind, 6 | 7) {
            Err("injected-frozen-descendant")
        } else {
            write_receipt(&env, &next)
        };
        if (buggy == 1 && result == Err("receipt-consumed-changed")) || (buggy == 2 && kind == 8) {
            // Negative control: the former raw writer can replace accepted or
            // unreadable custody with a stale caller's complete projection.
            super::super::super::super::upgrade_models::write_atomic_mode(
                &path,
                &aterm_json::to_string(&next).unwrap(),
                Some(0o600),
            )
            .unwrap();
            result = Ok(());
        }
        if buggy == 4 {
            result = Err("injected-permanent-refusal");
        }
        if buggy == 5 && result == Err("receipt-consumed-changed") {
            std::fs::write(&path, b"damaged after refusal").unwrap();
        }
        drop(failure);
        Ok((result, !recording.take().is_empty()))
    })
    .unwrap()
    .unwrap();
    let admitted = !matches!(
        result,
        Err("receipt-consumed-changed" | "journal-shape" | "injected-frozen-descendant")
    );
    let after = std::fs::read(&path).unwrap();
    let intact = if kind == 8 {
        after == before
    } else {
        aterm_json::from_slice::<Receipt>(&after).is_ok_and(|stored| {
            stored.phase == Phase::Consumed
                && stored.accepted_turn == root.accepted_turn
                && stored.same_intent(&root)
                && stored.current_attempt == root.current_attempt
                && stored.candidate == root.candidate
                && stored.successor_pid.is_some()
                && stored.successor_birth.is_some()
                && stored.launch_attempted
                && (!root.revoked || stored.revoked)
        })
    };
    let mut next = state.clone();
    next.insert("done", 1);
    next.insert("admitted", i64::from(admitted));
    next.insert("intact", i64::from(intact));
    next.insert("unchanged", i64::from(before == after));
    next.insert("acknowledged", i64::from(result.is_ok()));
    next.insert("barrier", i64::from(barrier));
    next
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "NextCase" => {
            next.insert("kind", state["kind"] + 1);
        }
        "PriorRevocation" => {
            next.insert("revoked", 1);
        }
        "NextFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Publish" => return publish_case(state, 0),
        _ => unreachable!(),
    }
    next
}

#[test]
fn consumed_publication_refines_actual_files_and_durability_barriers() {
    let model = aterm_models_harness::consumed_receipt_model();
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
    for (buggy, kind, revoked, fault, law) in [
        (1, 1, 0, 0, "AcceptedHistory"),
        (1, 2, 0, 0, "AcceptedHistory"),
        (1, 3, 0, 0, "AcceptedHistory"),
        (1, 4, 0, 0, "AcceptedHistory"),
        (1, 5, 1, 0, "AcceptedHistory"),
        (1, 9, 0, 0, "AcceptedHistory"),
        (1, 10, 0, 0, "AcceptedHistory"),
        (1, 11, 0, 0, "AcceptedHistory"),
        (1, 12, 0, 0, "AcceptedHistory"),
        (2, 8, 0, 0, "AcceptedHistory"),
        (3, 0, 0, 2, "DurableReturn"),
        (3, 0, 0, 0, "BarrierBeforeAcknowledgement"),
        (4, 0, 0, 0, "Available"),
        (5, 1, 0, 0, "RefusalPreservesBytes"),
        (6, 6, 0, 0, "Available"),
        (6, 7, 0, 0, "Available"),
    ] {
        let mut state = model.init_state();
        state.insert("kind", kind);
        state.insert("fault", fault);
        state.insert("revoked", revoked);
        let next = publish_case(&state, buggy);
        assert!(!model.check_invariant(law, &next), "{law}");
        assert!(!model.successors("Publish", &state).contains(&next));
    }
}

#[test]
fn consumed_publication_preserves_every_immutable_binding_and_accepted_turn() {
    let (_tmp, env, root) = consumed_fixture(false);
    let path = receipt_path(&env, &root.id).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut proposals = Vec::new();
    for phase in [
        Phase::Ready,
        Phase::Committing,
        Phase::Committed,
        Phase::Retired,
    ] {
        let mut next = root.clone();
        next.phase = phase;
        proposals.push(next);
    }
    for field in 0..17 {
        let mut next = root.clone();
        match field {
            0 => next.intent.thread.push('x'),
            1 => next.intent.wall.push('x'),
            2 => next.intent.identity = Identity::from_native("other", "user"),
            3 => {
                next.intent.purpose = Purpose::WeeklyQuota;
                next.destination = Some(Identity::from_native("target", "user"));
            }
            4 => next.sid.push('x'),
            5 => next.old_pid += 1,
            6 => next.old_birth += 1,
            7 => next.claim_revision += 1,
            8 => next.human_seq += 1,
            9 => next.expires_ms += 1,
            10 => next.old_auth.push("other"),
            11 => next.new_auth.push("other"),
            12 => next.generation += 1,
            13 => next.row_stamp.push('x'),
            14 => next.native_prepare.push('x'),
            15 => next.accepted_turn = None,
            16 => next.accepted_turn = Some("another turn".into()),
            _ => unreachable!(),
        }
        proposals.push(next);
    }
    with_receipt_writer(&env, |_| {
        for next in proposals {
            assert_eq!(write_receipt(&env, &next), Err("receipt-consumed-changed"));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        Ok(())
    })
    .unwrap()
    .unwrap();
}

#[test]
fn consumed_publication_keeps_visible_acceptance_after_lost_acknowledgement() {
    let (_tmp, env, mut consumed, _) = fixture();
    let stale = consumed.clone();
    consumed.phase = Phase::Consumed;
    consumed.accepted_turn = Some(consumed.intent.wall.clone());
    consumed.successor_pid = Some(40);
    consumed.successor_birth = Some(50);
    with_receipt_writer(&env, |writer| {
        let failure = aterm_durable::seam::FailingDirs::after(1, aterm_durable::seam::EIO);
        assert_eq!(
            write_receipt(&env, &consumed),
            Err("receipt-directory-flush")
        );
        drop(failure);
        assert_eq!(read_root(&env, &consumed.id), Ok(Some(consumed.clone())));
        let mut retired = stale.clone();
        retired.phase = Phase::Retired;
        retired.revoked = true;
        assert_eq!(
            write_receipt(&env, &retired),
            Err("receipt-consumed-changed")
        );
        assert_eq!(read_root(&env, &consumed.id), Ok(Some(consumed.clone())));
        assert_eq!(
            writer.cancel(&stale, CancellationKind::OwnerStop, 5, 5)?,
            None
        );
        assert_eq!(read_cancellation(&env, &consumed)?, None);
        let mut descendant = consumed.clone();
        descendant.successor_pid = Some(60);
        descendant.successor_birth = Some(70);
        write_receipt(&env, &descendant)?;
        assert_eq!(read_root(&env, &consumed.id)?, Some(descendant));
        Ok(())
    })
    .unwrap()
    .unwrap();
}

#[test]
fn consumed_publication_rejects_invalid_initial_shape_and_preserves_unattempted_history() {
    let (_tmp, env, root, _) = fixture();
    let path = receipt_path(&env, &root.id).unwrap();
    let before = std::fs::read(&path).unwrap();
    with_receipt_writer(&env, |_| {
        for case in 0..4 {
            let mut invalid = root.clone();
            invalid.phase = Phase::Consumed;
            invalid.accepted_turn = Some(root.intent.wall.clone());
            match case {
                0 => invalid.accepted_turn = None,
                1 => invalid.accepted_turn = Some("wrong turn".into()),
                2 => invalid.successor_pid = Some(40),
                3 => invalid.successor_birth = Some(50),
                _ => unreachable!(),
            }
            assert_eq!(write_receipt(&env, &invalid), Err("receipt-consumed-shape"));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        // Login observation can consume without setting the attempt flag.
        // Retaining that history does not prove native execution or closure.
        let mut consumed = root.clone();
        consumed.phase = Phase::Consumed;
        consumed.accepted_turn = Some(root.intent.wall.clone());
        consumed.launch_attempted = false;
        write_receipt(&env, &consumed)?;
        write_receipt(&env, &consumed)?;
        assert_eq!(read_root(&env, &root.id)?, Some(consumed));
        Ok(())
    })
    .unwrap()
    .unwrap();
}

#[test]
fn consumed_publication_preserves_oversized_retained_files() {
    let (_tmp, env, root) = consumed_fixture(false);
    let path = receipt_path(&env, &root.id).unwrap();
    let damaged = vec![b'x'; MAX_RECORD + 1];
    std::fs::write(&path, &damaged).unwrap();
    with_receipt_writer(&env, |_| {
        assert_eq!(write_receipt(&env, &root), Err("journal-size"));
        assert_eq!(std::fs::read(&path).unwrap(), damaged);
        Ok(())
    })
    .unwrap()
    .unwrap();
}

#[cfg(unix)]
#[test]
fn consumed_publication_preserves_unsafe_retained_files() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let (_tmp, env, root) = consumed_fixture(false);
    let path = receipt_path(&env, &root.id).unwrap();
    let before = std::fs::read(&path).unwrap();
    with_receipt_writer(&env, |_| {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(write_receipt(&env, &root), Err("journal-permissions"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let target = path.with_extension("retained");
        std::fs::rename(&path, &target).unwrap();
        symlink(&target, &path).unwrap();
        assert_eq!(write_receipt(&env, &root), Err("journal-open"));
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
        assert_eq!(std::fs::read(&target).unwrap(), before);
        Ok(())
    })
    .unwrap()
    .unwrap();
}
