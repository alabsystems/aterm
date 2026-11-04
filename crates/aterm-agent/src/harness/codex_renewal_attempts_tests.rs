// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::contract::Scope;
use crate::harness::native_act::{
    Image, NativePlan, Prepare, Process, Request as NativeRequest, RestartCommand,
};

fn fixture() -> (aterm_tempfile::TempDir, Env, Receipt, Prepare) {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let mut env = Env::from_process().unwrap();
    env.home = Some(tmp.path().join("home"));
    env.state = tmp.path().join("state");
    env.config = Some(tmp.path().join("aterm.toml"));
    let image = Image {
        path: b"/fixture/codex".to_vec(),
        device: 1,
        inode: 2,
    };
    let shell = Process {
        pid: 20,
        birth: 30,
        parent: 1,
        group: 20,
        session: 20,
        image: image.clone(),
    };
    let command = RestartCommand {
        image: image.clone(),
        program: "/fixture/codex".into(),
        argv: vec!["resume".into(), "original-thread".into()],
        env: vec![("CODEX_AUTH_PATH".into(), "/fixture/new/auth.json".into())],
        unset: None,
        cwd: Some("/fixture/work".into()),
        heal: None,
        dialect: crate::harness::upgrade::Dialect::Zsh,
    };
    let prepare = Prepare {
        id: "ab".repeat(16),
        host_epoch: "cd".repeat(16),
        holder: "fixture".into(),
        revision: 3,
        expected_human_seq: Some(4),
        deadline_us: 120_000_000,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            process: Process {
                pid: 10,
                birth: 11,
                parent: 20,
                group: 10,
                session: 20,
                image: image.clone(),
            },
            observation: None,
            shell,
            launcher: image,
            socket: "/fixture/control.sock".into(),
            forward: Box::new(command),
            rollback: None,
        },
    };
    let receipt = Receipt {
        destination: None,
        id: prepare.id.clone(),
        intent: Intent {
            purpose: Default::default(),
            thread: "019a4b54-5fa1-7123-8123-123456789abc".into(),
            wall: "019a4b54-5fa2-7123-8123-123456789abc".into(),
            identity: Identity::from_native("account", "user"),
        },
        sid: "s-fixture".into(),
        old_pid: 10,
        old_birth: 11,
        claim_revision: 3,
        human_seq: 4,
        old_auth: "/fixture/old/auth.json".into(),
        new_auth: "/fixture/new/auth.json".into(),
        generation: 2,
        row_stamp: "selected-row".into(),
        native_prepare: NativeRequest::prepare(prepare.clone()).encode().unwrap(),
        expires_ms: 120_000,
        phase: Phase::Committing,
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: true,
        candidate: None,
        current_attempt: None,
        revoked: false,
    };
    with_receipt_writer(&env, |_| write_receipt(&env, &receipt))
        .unwrap()
        .unwrap();
    (tmp, env, receipt, prepare)
}

fn child(parent: &Prepare, id: &str) -> Prepare {
    Prepare {
        id: id.repeat(16),
        revision: parent.revision + 1,
        expected_human_seq: Some(parent.expected_human_seq.unwrap() + 1),
        deadline_us: parent.deadline_us + 120_000_000,
        ..parent.clone()
    }
}

#[test]
fn attempt_chain_appends_immutable_recipes_without_growing_the_root_history() {
    let (_tmp, env, root, original) = fixture();
    let original_bytes = root.native_prepare.clone();
    assert_eq!(
        super::super::active_prepare(&env, &root),
        Some(original.clone())
    );
    let first_prepare = child(&original, "ef");
    let first = with_receipt_writer(&env, |writer| {
        let first = writer.propose(&root, &root.id, &first_prepare)?;
        let next = writer.publish(&root, &first)?;
        Ok((first, next))
    })
    .unwrap()
    .unwrap();
    let first_path = attempt_path(&env, &root.id, &first.0.reference.id).unwrap();
    let first_bytes = std::fs::read(&first_path).unwrap();
    assert_eq!(
        super::super::active_prepare(&env, &first.1),
        Some(first_prepare.clone())
    );
    let second_prepare = child(&first_prepare, "12");
    let second = with_receipt_writer(&env, |writer| {
        let second = writer.propose(&first.1, &first.0.reference.id, &second_prepare)?;
        writer.publish(&first.1, &second)
    })
    .unwrap()
    .unwrap();
    assert_eq!(second.id, root.id, "the Codex owed receipt remains stable");
    assert_eq!(second.native_prepare, original_bytes);
    assert_eq!(second.expires_ms, root.expires_ms);
    assert_eq!(
        std::fs::read(first_path).unwrap(),
        first_bytes,
        "old attempt bytes never change"
    );
    let active = read_active_attempt(&env, &second).unwrap().unwrap();
    assert_eq!(active.parent, first.0.reference);
    assert_eq!(active.reference.generation, 2);
    assert_eq!(
        NativeRequest::parse(&active.native_prepare),
        Some(NativeRequest::prepare(second_prepare.clone()))
    );
    assert_eq!(
        super::super::active_prepare(&env, &second),
        Some(second_prepare)
    );
    let encoded_root = aterm_json::to_string(&second).unwrap();
    assert_eq!(
        encoded_root.len(),
        aterm_json::to_string(&first.1).unwrap().len(),
        "appending the same-width reference does not add history to the root"
    );
}

#[test]
fn failed_root_publication_and_lost_ack_replay_the_exact_attempt() {
    let (_tmp, env, root, original) = fixture();
    let prepare = child(&original, "ef");
    let attempt = with_receipt_writer(&env, |writer| {
        let attempt = writer.propose(&root, &root.id, &prepare)?;
        assert_eq!(
            writer.publish_using(&root, &attempt, |_| Err("injected-before-root")),
            Err("injected-before-root")
        );
        assert!(
            read_receipt(&env, &root.id)
                .unwrap()
                .current_attempt
                .is_none()
        );
        Ok(attempt)
    })
    .unwrap()
    .unwrap();
    let path = attempt_path(&env, &root.id, &attempt.reference.id).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    with_receipt_writer(&env, |writer| {
        assert_eq!(
            writer.publish_using(&root, &attempt, |next| {
                write_receipt(&env, next)?;
                Err("injected-lost-root-ack")
            }),
            Err("injected-lost-root-ack")
        );
        let replayed = writer.publish(&root, &attempt)?;
        assert_eq!(replayed.current_attempt.as_ref(), Some(&attempt.reference));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        Ok(())
    })
    .unwrap()
    .unwrap();
}

#[test]
fn concurrent_coordinators_publish_only_one_child_of_the_same_root() {
    let (_tmp, env, root, original) = fixture();
    let first_prepare = child(&original, "ef");
    let second_prepare = child(&original, "12");
    let start = std::sync::Barrier::new(3);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            start.wait();
            with_receipt_writer(&env, |writer| {
                writer.publish(&root, &writer.propose(&root, &root.id, &first_prepare)?)
            })
        });
        let second = scope.spawn(|| {
            start.wait();
            with_receipt_writer(&env, |writer| {
                writer.publish(&root, &writer.propose(&root, &root.id, &second_prepare)?)
            })
        });
        start.wait();
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(Some(_))))
            .count(),
        1
    );
    let current = read_receipt(&env, &root.id).unwrap();
    let active = read_active_attempt(&env, &current).unwrap().unwrap();
    assert_eq!(active.reference.generation, 1);
    assert_eq!(active.parent.id, root.id);
    assert_eq!(
        std::fs::read_dir(
            attempt_path(&env, &root.id, &active.reference.id)
                .unwrap()
                .parent()
                .unwrap()
        )
        .unwrap()
        .count(),
        1
    );
}

#[test]
fn durable_cancellation_survives_stale_root_projection_and_blocks_all_new_attempts() {
    let (_tmp, env, root, original) = fixture();
    let tombstone = with_receipt_writer(&env, |writer| {
        writer.cancel(&root, CancellationKind::OwnerStop, 5, 5)
    })
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(tombstone.reason, CancellationKind::OwnerStop);
    let retired = read_receipt(&env, &root.id).unwrap();
    assert!(retired.revoked);
    assert!(!retired.may_replace());
    assert_eq!(retired.phase, Phase::Retired);
    with_receipt_writer(&env, |writer| {
        assert_eq!(write_receipt(&env, &root), Err("receipt-revoked"));
        assert_eq!(
            writer.publish(
                &root,
                &Attempt {
                    root: root.id.clone(),
                    reference: AttemptRef {
                        id: "ef".repeat(16),
                        generation: 1
                    },
                    parent: AttemptRef {
                        id: root.id.clone(),
                        generation: 0
                    },
                    native_predecessor: root.id.clone(),
                    native_prepare: NativeRequest::prepare(child(&original, "ef"))
                        .encode()
                        .unwrap()
                }
            ),
            Err("attempt-revoked")
        );
        assert_eq!(
            writer.cancel(&root, CancellationKind::NewPersonTask, 6, 6)?,
            Some(tombstone.clone())
        );
        Ok(())
    })
    .unwrap()
    .unwrap();
    // A durable tombstone also wins if a crash or an older writer left the old
    // projection on disk. This deliberate fault bypasses the production writer.
    super::super::super::upgrade_models::write_atomic_mode(
        &receipt_path(&env, &root.id).unwrap(),
        &aterm_json::to_string(&root).unwrap(),
        Some(0o600),
    )
    .unwrap();
    let projected = read_receipt(&env, &root.id).unwrap();
    assert!(projected.revoked);
    assert_eq!(projected.phase, Phase::Retired);
    assert_eq!(
        read_cancellation(&env, &projected).unwrap(),
        Some(tombstone)
    );
}

#[test]
fn changed_recipe_parent_or_intent_never_publishes_an_attempt() {
    let (_tmp, env, root, original) = fixture();
    with_receipt_writer(&env, |writer| {
        let mut changed = child(&original, "ef");
        if let NativePlan::Restart { forward, .. } = &mut changed.plan {
            forward.cwd = Some("/other-workspace".into());
        }
        assert_eq!(
            writer.propose(&root, &root.id, &changed),
            Err("attempt-recipe-changed")
        );
        assert_eq!(
            writer.propose(&root, &"99".repeat(16), &child(&original, "ef")),
            Err("attempt-predecessor")
        );
        let mut terminal = root.clone();
        terminal.accepted_turn = Some(terminal.intent.wall.clone());
        assert_eq!(
            writer.propose(&terminal, &root.id, &child(&original, "ef")),
            Err("attempt-intent-terminal")
        );
        let mut not_attempted = root.clone();
        not_attempted.launch_attempted = false;
        assert_eq!(
            writer.propose(&not_attempted, &root.id, &child(&original, "ef")),
            Err("attempt-intent-terminal")
        );
        Ok(())
    })
    .unwrap()
    .unwrap();
    assert!(
        read_receipt(&env, &root.id)
            .unwrap()
            .current_attempt
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn immutable_attempt_and_tombstone_readers_reject_links_and_never_touch_the_target() {
    use std::os::unix::fs::symlink;
    let (tmp, env, root, original) = fixture();
    let target = tmp.path().join("untouched");
    std::fs::write(&target, b"fixture sentinel").unwrap();
    with_receipt_writer(&env, |writer| {
        let attempt = writer.propose(&root, &root.id, &child(&original, "ef"))?;
        let path = attempt_path(&env, &root.id, &attempt.reference.id)?;
        private_parent(&path)?;
        symlink(&target, &path).unwrap();
        assert!(writer.publish(&root, &attempt).is_err());
        assert!(
            read_receipt(&env, &root.id)
                .unwrap()
                .current_attempt
                .is_none()
        );
        Ok(())
    })
    .unwrap()
    .unwrap();
    symlink(&target, cancellation_path(&env, &root.id).unwrap()).unwrap();
    assert!(
        read_receipt(&env, &root.id).is_none(),
        "unreadable custody never becomes a missing cancellation"
    );
    assert_eq!(std::fs::read(target).unwrap(), b"fixture sentinel");
}

#[test]
fn changed_immediate_parent_recipe_makes_the_current_journal_unavailable() {
    let (_tmp, env, root, original) = fixture();
    let first_prepare = child(&original, "ef");
    let (mut first, second) = with_receipt_writer(&env, |writer| {
        let first = writer.propose(&root, &root.id, &first_prepare)?;
        let first_root = writer.publish(&root, &first)?;
        let second = writer.propose(
            &first_root,
            &first.reference.id,
            &child(&first_prepare, "12"),
        )?;
        let second_root = writer.publish(&first_root, &second)?;
        Ok((first, second_root))
    })
    .unwrap()
    .unwrap();
    let mut changed = first_prepare;
    if let NativePlan::Restart { forward, .. } = &mut changed.plan {
        forward.cwd = Some("/other-workspace".into());
    }
    first.native_prepare = NativeRequest::prepare(changed).encode().unwrap();
    // Deliberate external corruption, not a permitted immutable journal write.
    super::super::super::upgrade_models::write_atomic_mode(
        &attempt_path(&env, &root.id, &first.reference.id).unwrap(),
        &aterm_json::to_string(&first).unwrap(),
        Some(0o600),
    )
    .unwrap();
    assert_eq!(
        read_active_attempt(&env, &second),
        Err("attempt-parent-recipe")
    );
}
