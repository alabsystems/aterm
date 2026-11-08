// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::native_act::Request as NativeRequest;

fn fixture() -> (aterm_tempfile::TempDir, Env, Receipt, IndexKey) {
    let (tmp, env, mut root, mut prepare) = super::super::tests::fixture();
    root.id = "12".repeat(16);
    prepare.id.clone_from(&root.id);
    root.native_prepare = NativeRequest::prepare(prepare).encode().unwrap();
    root.phase = Phase::Ready;
    root.launch_attempted = false;
    let key = IndexKey::login(root.old_pid, "start", &root.intent.wall, root.generation);
    (tmp, env, root, key)
}

fn other(root: &Receipt, phase: Phase) -> Receipt {
    let mut old = root.clone();
    old.id = "34".repeat(16);
    let Some(NativeRequest::Prepare(mut prepare)) = NativeRequest::parse(&old.native_prepare)
    else {
        panic!("fixture prepare");
    };
    prepare.id.clone_from(&old.id);
    old.native_prepare = NativeRequest::prepare(*prepare).encode().unwrap();
    old.phase = phase;
    old.accepted_turn = (old.phase == Phase::Consumed).then(|| old.intent.wall.clone());
    old
}

fn put(path: &Path, text: &str) {
    private_parent(path).unwrap();
    super::super::super::super::upgrade_models::write_atomic_mode(path, text, Some(0o600)).unwrap();
}

fn run_case(state: &aterm_spec::interp::State, buggy: u8) -> (bool, bool) {
    let (_tmp, env, initial, mut key) = fixture();
    let scenario = state["scenario"];
    if scenario == 6 {
        let mut intent = initial.intent.clone();
        intent.purpose = Purpose::WeeklyQuota;
        key = IndexKey::quota(&intent, initial.old_pid, initial.old_birth, "start").unwrap();
    }
    let path = key.path(&env).unwrap();
    let prior = match scenario {
        0 => None,
        1 | 6 => {
            with_receipt_writer(&env, |writer| writer.create(&initial))
                .unwrap()
                .unwrap();
            put(&path, &initial.id);
            None
        }
        2 | 3 | 7 => {
            let old = other(
                &initial,
                if scenario == 3 {
                    Phase::Ready
                } else {
                    Phase::Retired
                },
            );
            let mut stored = old.clone();
            if scenario == 7 {
                stored.claim_revision += 1;
            }
            write_receipt(&env, &stored).unwrap();
            put(&path, &old.id);
            Some(old)
        }
        4 => {
            put(&path, "{");
            None
        }
        5 => {
            put(&path, &"34".repeat(16));
            None
        }
        _ => unreachable!(),
    };
    let before = std::fs::read(&path).ok();
    let accepted = with_receipt_writer(&env, |writer| {
        let after = match scenario {
            0 => 2,
            2 => 3,
            _ => 0,
        };
        let failure = (state["failed"] == 1)
            .then(|| aterm_durable::seam::FailingDirs::after(after, aterm_durable::seam::EIO));
        let result = if buggy == 1 && scenario <= 2 {
            // The former publisher omitted the last directory barrier, and its
            // visible-index retry returned after an observational read alone.
            if scenario == 1 {
                key.observe(&env).map(|_| ())
            } else {
                let _ = writer.read_indexed(&key)?;
                writer.create(&initial).and_then(|()| {
                    super::super::super::super::upgrade_models::write_atomic_mode(
                        &path,
                        &initial.id,
                        Some(0o600),
                    )
                    .map_err(|_| "index-write")
                })
            }
        } else if buggy == 2 && matches!(scenario, 3 | 7) {
            writer.create(&initial).and_then(|()| {
                super::super::super::super::upgrade_models::write_atomic_mode(
                    &path,
                    &initial.id,
                    Some(0o600),
                )
                .map_err(|_| "index-write")
            })
        } else if buggy == 3 {
            Err("disabled")
        } else {
            writer.publish_indexed(&key, prior.as_ref(), &initial)
        };
        drop(failure);
        Ok(result.is_ok())
    })
    .unwrap()
    .unwrap();
    (
        accepted,
        scenario <= 2 || std::fs::read(&path).ok() == before,
    )
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Scenario" => {
            next.insert("scenario", state["scenario"] + 1);
        }
        "Failed" => {
            next.insert("failed", 1);
        }
        "Publish" => {
            let (accepted, preserved) = run_case(state, 0);
            next.insert("done", 1);
            next.insert("accepted", i64::from(accepted));
            next.insert("preserved", i64::from(preserved));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn receipt_index_refines_publication_and_retry_barriers() {
    let model = aterm_models_harness::receipt_index_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&transition(&state, action.name)),
                    "{state:?}"
                );
            }
        }
    }
    for (buggy, scenario, failed, law) in [
        (1, 0, 1, "Acknowledged"),
        (1, 1, 1, "Acknowledged"),
        (2, 3, 0, "Unambiguous"),
        (2, 3, 0, "PreserveForbidden"),
        (2, 7, 0, "Unambiguous"),
        (3, 0, 0, "Available"),
    ] {
        let mut state = model.init_state();
        state.insert("scenario", scenario);
        state.insert("failed", failed);
        let (accepted, preserved) = run_case(&state, buggy);
        let mut wrong = state.clone();
        wrong.insert("done", 1);
        wrong.insert("accepted", i64::from(accepted));
        wrong.insert("preserved", i64::from(preserved));
        assert!(!model.check_invariant(law, &wrong), "{law}");
        assert!(!model.successors("Publish", &state).contains(&wrong));
    }
}

#[test]
fn receipt_index_repairs_visible_rename_and_never_acknowledges_failed_retry() {
    let (_tmp, env, initial, key) = fixture();
    with_receipt_writer(&env, |writer| {
        let failed = aterm_durable::seam::FailingDirs::after(2, aterm_durable::seam::EIO);
        assert!(writer.publish_indexed(&key, None, &initial).is_err());
        drop(failed);
        assert_eq!(key.observe(&env)?, Some(initial.id.clone()));
        let failed = aterm_durable::seam::FailingDirs::after(0, aterm_durable::seam::EIO);
        assert!(writer.read_indexed(&key).is_err());
        assert!(writer.publish_indexed(&key, None, &initial).is_err());
        drop(failed);
        for after in 0..3 {
            let failed = aterm_durable::seam::FailingDirs::after(after, aterm_durable::seam::EIO);
            assert!(writer.publish_indexed(&key, None, &initial).is_err());
            drop(failed);
        }
        writer.publish_indexed(&key, None, &initial)?;
        assert_eq!(writer.read_indexed(&key)?, Some(initial.clone()));
        Ok(())
    })
    .unwrap()
    .unwrap();
}

#[test]
fn receipt_index_keeps_reference_when_root_acknowledgement_fails() {
    let (_tmp, env, initial, key) = fixture();
    with_receipt_writer(&env, |writer| {
        let failed = aterm_durable::seam::FailingDirs::after(1, aterm_durable::seam::EIO);
        assert!(writer.publish_indexed(&key, None, &initial).is_err());
        drop(failed);
        assert_eq!(key.observe(&env)?, None);
        assert_eq!(read_receipt(&env, &initial.id), Some(initial.clone()));
        writer.publish_indexed(&key, None, &initial)?;
        Ok(())
    })
    .unwrap()
    .unwrap();
}

#[test]
fn receipt_index_preserves_active_and_cancelled_roots() {
    for state in 0..5 {
        let (_tmp, env, initial, key) = fixture();
        let mut old = other(
            &initial,
            match state {
                0 => Phase::Committing,
                1 => Phase::Committed,
                2 => Phase::Consumed,
                3 => Phase::Retired,
                _ => Phase::Ready,
            },
        );
        if state == 3 {
            old.launch_attempted = true;
        }
        write_receipt(&env, &old).unwrap();
        put(&key.path(&env).unwrap(), &old.id);
        with_receipt_writer(&env, |writer| {
            if state == 4 {
                writer.cancel(&old, CancellationKind::OwnerStop, 1, 1)?;
            }
            let current = writer.read_indexed(&key)?.unwrap();
            let root_path = receipt_path(&env, &old.id).unwrap();
            let before = std::fs::read(&root_path).unwrap();
            assert!(
                writer
                    .publish_indexed(&key, Some(&current), &initial)
                    .is_err()
            );
            assert_eq!(key.observe(&env)?, Some(old.id.clone()));
            assert_eq!(std::fs::read(root_path).unwrap(), before);
            assert_eq!(read_receipt(&env, &initial.id), None);
            Ok(())
        })
        .unwrap()
        .unwrap();
    }
}

#[test]
fn receipt_index_rejects_unsafe_files_and_uses_the_writers_store() {
    for case in 0..6 {
        let (_tmp, env, initial, key) = fixture();
        let path = key.path(&env).unwrap();
        match case {
            0 => put(&path, &"AB".repeat(16)),
            1 => put(&path, &"ab".repeat(17)),
            2 => put(&path, &"z".repeat(32)),
            3 => {
                private_parent(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
            4 => {
                #[cfg(unix)]
                {
                    private_parent(&path).unwrap();
                    std::os::unix::fs::symlink(path.with_extension("missing"), &path).unwrap();
                }
                #[cfg(not(unix))]
                {
                    private_parent(&path).unwrap();
                    std::fs::create_dir(&path).unwrap();
                }
            }
            5 => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    put(&path, &initial.id);
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                #[cfg(not(unix))]
                {
                    put(&path, "invalid");
                }
            }
            _ => unreachable!(),
        }
        assert!(key.observe(&env).is_err());
        with_receipt_writer(&env, |writer| {
            assert!(writer.read_indexed(&key).is_err());
            assert!(writer.publish_indexed(&key, None, &initial).is_err());
            Ok(())
        })
        .unwrap()
        .unwrap();
    }
    let (_first, env, initial, key) = fixture();
    let (_second, other_env, _, _) = fixture();
    with_receipt_writer(&env, |writer| writer.publish_indexed(&key, None, &initial))
        .unwrap()
        .unwrap();
    assert_eq!(key.observe(&other_env).unwrap(), None);
    assert_eq!(key.observe(&env).unwrap(), Some(initial.id));
}
