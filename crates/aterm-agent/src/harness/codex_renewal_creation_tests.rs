// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn initial() -> (aterm_tempfile::TempDir, Env, Receipt) {
    let (tmp, env, mut root, mut prepare) = fixture();
    root.id = "12".repeat(16);
    prepare.id.clone_from(&root.id);
    root.native_prepare = NativeRequest::prepare(prepare).encode().unwrap();
    root.phase = Phase::Ready;
    root.launch_attempted = false;
    (tmp, env, root)
}

fn snapshot(path: &Path) -> (u8, Vec<u8>) {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (0, Vec::new()),
        Err(e) => panic!("{e}"),
        Ok(m) if m.file_type().is_symlink() => (
            1,
            std::fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
        ),
        Ok(m) if m.is_dir() => (2, Vec::new()),
        Ok(_) => (3, std::fs::read(path).unwrap()),
    }
}

fn run_case(state: &aterm_spec::interp::State, buggy: i64) -> (bool, bool) {
    let (_tmp, env, root) = initial();
    let path = receipt_path(&env, &root.id).unwrap();
    private_parent(&path).unwrap();
    match state["existing"] {
        0 => {}
        1 => write_immutable(&path, &root).unwrap(),
        2 => {
            let mut other = root.clone();
            other.generation += 1;
            write_immutable(&path, &other).unwrap();
        }
        3 => {
            super::super::super::super::upgrade_models::write_atomic_mode(&path, "{", Some(0o600))
                .unwrap();
        }
        4 => {
            #[cfg(unix)]
            std::os::unix::fs::symlink(path.with_extension("missing"), &path).unwrap();
            #[cfg(not(unix))]
            std::fs::create_dir(&path).unwrap();
        }
        5 => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                write_immutable(&path, &root).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            #[cfg(not(unix))]
            std::fs::create_dir(&path).unwrap();
        }
        6 => {
            super::super::super::super::upgrade_models::write_atomic_mode(
                &path,
                &"x".repeat(MAX_RECORD + 1),
                Some(0o600),
            )
            .unwrap();
        }
        7 => std::fs::create_dir(&path).unwrap(),
        _ => unreachable!(),
    }
    let cancellation = cancellation_path(&env, &root.id).unwrap();
    match state["cancelled"] {
        0 => {}
        1 => write_immutable(
            &cancellation,
            &Cancellation {
                root: root.id.clone(),
                intent: root.intent.clone(),
                reason: CancellationKind::OwnerStop,
                human_seq: 5,
                reached_seq: 5,
            },
        )
        .unwrap(),
        2 => super::super::super::super::upgrade_models::write_atomic_mode(
            &cancellation,
            "{",
            Some(0o600),
        )
        .unwrap(),
        _ => unreachable!(),
    }
    let before = snapshot(&path);
    let cancel_before = snapshot(&cancellation);
    let accepted = with_receipt_writer(&env, |writer| {
        let failure = (state["failed"] != 0)
            .then(|| aterm_durable::seam::FailingDirs::after(1, aterm_durable::seam::EIO));
        let result = if buggy == 1 && state["existing"] == 2 {
            write_receipt(&env, &root)
        } else if buggy == 2 {
            write_immutable(&path, &root)
        } else {
            writer.create(&root)
        };
        drop(failure);
        Ok(
            if buggy == 3 && state["existing"] <= 1 && state["cancelled"] == 0 {
                true
            } else if buggy == 4 {
                false
            } else {
                result.is_ok()
            },
        )
    })
    .unwrap()
    .unwrap();
    let preserved =
        (before.0 == 0 || snapshot(&path) == before) && snapshot(&cancellation) == cancel_before;
    (accepted, preserved)
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Existing" => {
            next.insert("existing", state["existing"] + 1);
        }
        "Cancellation" => {
            next.insert("cancelled", state["cancelled"] + 1);
        }
        "Failed" => {
            next.insert("failed", 1);
        }
        "Create" => {
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
fn receipt_creation_refines_real_artifact_retention_and_barriers() {
    let model = aterm_models_harness::receipt_creation_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {next:?}"
                );
            }
        }
    }
    for (buggy, existing, cancelled, failed, law) in [
        (1, 2, 0, 0, "PreserveArtifact"),
        (1, 2, 0, 0, "ExactCreation"),
        (2, 0, 1, 0, "RespectCancellation"),
        (2, 0, 2, 0, "RespectCancellation"),
        (3, 0, 0, 1, "Acknowledged"),
        (4, 0, 0, 0, "Available"),
    ] {
        let mut state = model.init_state();
        state.insert("existing", existing);
        state.insert("cancelled", cancelled);
        state.insert("failed", failed);
        let (accepted, preserved) = run_case(&state, buggy);
        let mut wrong = state.clone();
        wrong.insert("done", 1);
        wrong.insert("accepted", i64::from(accepted));
        wrong.insert("preserved", i64::from(preserved));
        assert!(!model.check_invariant(law, &wrong), "{law}");
        assert!(!model.successors("Create", &state).contains(&wrong));
    }
}

#[test]
fn receipt_creation_retries_exact_bytes_after_uncertain_acknowledgement() {
    let (_tmp, env, root) = initial();
    let path = receipt_path(&env, &root.id).unwrap();
    with_receipt_writer(&env, |writer| {
        let failure = aterm_durable::seam::FailingDirs::after(1, aterm_durable::seam::EIO);
        assert!(writer.create(&root).is_err());
        drop(failure);
        assert_eq!(read_receipt(&env, &root.id), Some(root.clone()));
        let before = snapshot(&path);
        writer.create(&root)?;
        assert_eq!(snapshot(&path), before);
        Ok(())
    })
    .unwrap()
    .unwrap();
}

#[test]
fn receipt_creation_refuses_progress_or_invalid_identity() {
    let (_tmp, env, initial) = initial();
    let path = receipt_path(&env, &initial.id).unwrap();
    for bad in 0..10 {
        let mut root = initial.clone();
        match bad {
            0 => root.phase = Phase::Committing,
            1 => root.revoked = true,
            2 => root.launch_attempted = true,
            3 => {
                root.current_attempt = Some(AttemptRef {
                    id: "34".repeat(16),
                    generation: 1,
                })
            }
            4 => {
                root.candidate = Some(Candidate::of(
                    &Process {
                        pid: 10,
                        birth: 11,
                        parent: 20,
                        group: 10,
                        session: 20,
                        image: Image {
                            path: b"/fixture/codex".to_vec(),
                            device: 1,
                            inode: 2,
                        },
                    },
                    true,
                ))
            }
            5 => root.successor_pid = Some(10),
            6 => root.successor_birth = Some(11),
            7 => root.accepted_turn = Some(root.intent.wall.clone()),
            8 => root.destination = Some(Identity::from_native("other", "user")),
            _ => root.intent.identity.account = "invalid".into(),
        }
        assert_eq!(
            with_receipt_writer(&env, |writer| writer.create(&root)),
            Err("receipt-not-initial")
        );
        assert_eq!(snapshot(&path).0, 0);
    }
}
