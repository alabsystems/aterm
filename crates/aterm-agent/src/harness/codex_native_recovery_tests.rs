// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Synthetic issuer and zero external resources, actual counted retirement,
//! canonical writer, encrypted owner, kernel publication roles and CLI recovery.
//! No provider/browser/independent-lineage qualification is claimed.

use super::super::{account_work, cli, codex_grant_owner, codex_native_row};
use super::*;

struct LocalOnly(i64);
impl cli::AccountsHost for LocalOnly {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<super::super::align::Captured, String> {
        panic!("no provider request")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login replay")
    }
    fn now(&self) -> i64 {
        self.0
    }
}

fn retire_original(env: &cli::Env, request: &Request, lifetime: &account_work::Lifetime) {
    let (work, control) = owned_work(request.clone(), lifetime.enter().unwrap());
    // Test fixture created the admitted attempt with no browser/child resources.
    // Preserve the real Scope drop and canonical original-retirement transition.
    work.admitted
        .store(true, std::sync::atomic::Ordering::Release);
    drop(work);
    control.retired().unwrap().finish(env).unwrap();
}

fn admission(work: &Work) -> Admission<'_> {
    Admission {
        scope: &work.scope,
        request: &work.request,
        original: &work.admitted,
        recovery: &work.recovery,
    }
}

#[test]
fn native_fenced_owner_only_recovery_publishes_both_rows_without_exchange() {
    codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
        let lifetime = account_work::Lifetime::default();
        let mut rotated = std::collections::BTreeSet::new();
        for (request, staging, fresh, _) in rows {
            assert!(!staging.directory.join("auth.json").exists());
            retire_original(env, &request, &lifetime);
            let expected = codex_grant_owner::with_retained_response(
                env,
                &request,
                &fresh,
                |_, doc, _, _, _| {
                    Ok(aterm_codec::hex::encode_lower(
                        &aterm_digest::Sha256::digest(doc),
                    ))
                },
            )
            .unwrap();
            let (work, _) = owned_work(request.clone(), lifetime.enter().unwrap());
            let retirement = std::sync::Mutex::new(None);
            let context = NativeContext {
                env,
                runtime: &work.native,
                scope: &work.scope,
                retirement: &retirement,
            };
            let status = cli::enroll_codex_operation_custody(
                env,
                &LocalOnly(env.now),
                &request,
                Some(&admission(&work)),
                Some(&context),
            )
            .unwrap();
            assert_eq!(status.phase, Phase::Applied);
            let records = store.peek_account_records(roster, env.now).unwrap();
            let binding = records
                .logins
                .enrollment
                .published_grant(&request.label, status.generation.unwrap())
                .unwrap();
            let witness = records
                .logins
                .enrollment
                .owner_witness(&binding)
                .unwrap()
                .unwrap();
            assert_eq!(witness.revision.generation, 1);
            assert_eq!(witness.revision.digest, expected);
            assert!(witness.pending.is_none());
            assert_eq!(
                aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
                    std::fs::read(staging.directory.join("auth.json")).unwrap()
                )),
                expected
            );
            // Promote a synthetic received refresh through the actual encrypted
            // journal + canonical witness writer, then replay the original op.
            let refresh_scope = lifetime.enter().unwrap();
            codex_grant_owner::fixture_promote_received_response(env, &binding, &refresh_scope);
            let advanced = store.peek_account_records(roster, env.now).unwrap();
            let c2 = advanced
                .logins
                .enrollment
                .owner_witness(&binding)
                .unwrap()
                .unwrap();
            assert_eq!(c2.revision.generation, 2);
            assert_ne!(c2.revision.digest, expected);
            let c2_document = std::fs::read(staging.directory.join("auth.json")).unwrap();
            assert_eq!(
                aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&c2_document)),
                c2.revision.digest
            );
            assert!(
                rotated.insert(
                    super::super::codex_app_login::owned_refresh_token_digest(&c2_document)
                        .unwrap()
                ),
                "independent fixture bundles must not manufacture a shared refresh token"
            );
            let before_replay = advanced.logins.to_json();
            let replay = cli::enroll_codex_operation_custody(
                env,
                &LocalOnly(env.now),
                &request,
                Some(&admission(&work)),
                Some(&context),
            )
            .unwrap();
            assert_eq!(replay, status);
            let after_replay = store.peek_account_records(roster, env.now).unwrap();
            assert_eq!(after_replay.logins.to_json(), before_replay);
            assert_eq!(
                after_replay
                    .logins
                    .enrollment
                    .owner_witness(&binding)
                    .unwrap()
                    .unwrap(),
                c2
            );
            assert_eq!(
                std::fs::read(staging.directory.join("auth.json")).unwrap(),
                c2_document
            );
        }
        assert_eq!(
            store
                .peek_account_records(roster, env.now)
                .unwrap()
                .logins
                .generation_clock,
            2
        );
    });
}

#[test]
fn native_recovery_resources_supersede_only_native_counter_and_reject_stale_retirement() {
    codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
        let lifetime = account_work::Lifetime::default();
        let [(request, _, fresh, _), (legacy, _, legacy_fresh, _)] = rows;
        assert!(
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).is_err()
        );
        retire_original(env, &request, &lifetime);
        let stopped = lifetime.enter().unwrap();
        stopped.cancellation().cancel();
        let before_wait = store
            .peek_account_records(roster, env.now)
            .unwrap()
            .logins
            .to_json();
        let stopped_candidate =
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).unwrap();
        assert!(
            codex_grant_owner::admit_native_recovery_publication(
                env,
                store,
                stopped_candidate,
                &fresh,
                &stopped,
                super::super::wake_deadline::WakeDeadline::after(super::super::login_drive::HANG)
            )
            .is_err()
        );
        assert_eq!(
            store
                .peek_account_records(roster, env.now)
                .unwrap()
                .logins
                .to_json(),
            before_wait
        );
        drop(stopped);
        let deadline =
            super::super::wake_deadline::WakeDeadline::after(super::super::login_drive::HANG);
        let (first, first_control) = owned_work(request.clone(), lifetime.enter().unwrap());
        let candidate =
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).unwrap();
        let resources = codex_grant_owner::admit_native_recovery_publication(
            env,
            store,
            candidate,
            &fresh,
            &first.scope,
            deadline,
        )
        .unwrap();
        let first_turn = codex_native_row::claim_retired_recovery(
            env,
            (store, roster),
            resources,
            &fresh,
            &admission(&first),
        )
        .unwrap();
        assert_eq!(first_turn.admission().claim(), 1);
        drop(first_turn);
        drop(first); // Simulated sender loss: no canonical retirement is published.
        let stale = first_control.retired().unwrap();
        let (second, _) = owned_work(request.clone(), lifetime.enter().unwrap());
        let candidate =
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).unwrap();
        let resources = codex_grant_owner::admit_native_recovery_publication(
            env,
            store,
            candidate,
            &fresh,
            &second.scope,
            deadline,
        )
        .unwrap();
        let second_turn = codex_native_row::claim_retired_recovery(
            env,
            (store, roster),
            resources,
            &fresh,
            &admission(&second),
        )
        .unwrap();
        assert_eq!(second_turn.admission().claim(), 2);
        let records = store.peek_account_records(roster, env.now).unwrap();
        assert!(
            records
                .logins
                .enrollment
                .require_recovery(&request, 1)
                .is_err()
        );
        records
            .logins
            .enrollment
            .require_native_publication_recovery(&request, second_turn.admission().ownership())
            .unwrap();
        assert!(stale.finish(env).is_err());
        drop(second_turn);
        drop(second);
        retire_original(env, &legacy, &lifetime);
        store
            .transact_logins(env.now, &mut None, |_, logins| {
                logins.enrollment.claim_recovery(&legacy)?;
                Ok(Vec::new())
            })
            .unwrap();
        assert!(
            codex_native_row::recovery_candidate(env, (store, roster), &legacy, &legacy_fresh)
                .is_err()
        );
    });
}

#[test]
fn cancelled_native_recovery_only_retires_under_actual_cleanup_count() {
    codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
        let [(request, staging, fresh, _), _] = rows;
        let lifetime = account_work::Lifetime::default();
        retire_original(env, &request, &lifetime);
        let deadline =
            super::super::wake_deadline::WakeDeadline::after(super::super::login_drive::HANG);
        let (first, first_control) = owned_work(request.clone(), lifetime.enter().unwrap());
        let candidate =
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).unwrap();
        let resources = codex_grant_owner::admit_native_recovery_publication(
            env,
            store,
            candidate,
            &fresh,
            &first.scope,
            deadline,
        )
        .unwrap();
        let turn = codex_native_row::claim_retired_recovery(
            env,
            (store, roster),
            resources,
            &fresh,
            &admission(&first),
        )
        .unwrap();
        assert_eq!(
            first_control.cancel(env).unwrap().phase,
            Phase::CancelRequested
        );
        drop(turn);
        drop(first); // No canonical retirement from this simulated lost sender.
        let old_retirement = first_control.retired().unwrap();
        let (probe, probe_control) = owned_work(request.clone(), lifetime.enter().unwrap());
        let candidate =
            super::super::codex_native_retirement::candidate(env, store, &request, &fresh).unwrap();
        let release = probe.scope.release_turn().unwrap();
        let resources = codex_grant_owner::admit_native_retirement(
            env,
            store,
            candidate,
            &fresh,
            &probe.scope,
            release,
            deadline,
        )
        .unwrap();
        drop(probe);
        assert!(
            !probe_control.work_retired(),
            "cleanup kernel resources must retain their actual Scope"
        );
        drop(resources);
        assert!(probe_control.work_retired());
        assert!(
            probe_control.retired().is_none(),
            "resource observation without a claim cannot mint retirement"
        );
        let reference = fresh.reference();
        assert!(!staging.directory.join("auth.json").exists());
        let before_files: std::collections::BTreeSet<_> = std::fs::read_dir(&staging.directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let (work, control) = owned_work(request.clone(), lifetime.enter().unwrap());
        let (other_work, other_control) = owned_work(request.clone(), lifetime.enter().unwrap());
        let pause = lifetime.pause().unwrap();
        assert!(work.scope.cancelled());
        let retirement = std::sync::Mutex::new(None);
        let context = NativeContext {
            env,
            runtime: &work.native,
            scope: &work.scope,
            retirement: &retirement,
        };
        let status = cli::enroll_codex_operation_custody(
            env,
            &LocalOnly(env.now),
            &request,
            Some(&admission(&work)),
            Some(&context),
        )
        .unwrap();
        assert_eq!(status.phase, Phase::CancelRequested);
        let records = store.peek_account_records(roster, env.now).unwrap();
        let claim = records
            .logins
            .enrollment
            .0
            .get(&request.id)
            .unwrap()
            .recovery
            .as_ref()
            .unwrap();
        assert_eq!(claim.generation, 2);
        assert_eq!(claim.kind, Some(RecoveryKind::NativeRetirement));
        assert!(!claim.retired);
        let retirement_receipt = work.recovery.lock().unwrap().as_ref().unwrap().clone();
        assert!(
            admission(&other_work)
                .native_recovery(&retirement_receipt)
                .is_err(),
            "cleanup identity cannot move to another cancelled Scope"
        );
        drop(other_work);
        assert!(other_control.retired().is_none());

        assert!(
            records
                .logins
                .enrollment
                .require_native_publication_recovery(&request, &retirement_receipt)
                .is_err()
        );
        let mut forged = records.logins.enrollment.clone();
        forged.0.get_mut(&request.id).unwrap().status.phase = Phase::Running;
        assert!(
            forged
                .commit(
                    &records.logins.enrollment,
                    &Default::default(),
                    &Default::default()
                )
                .is_err(),
            "cleanup cannot revive publication"
        );

        assert!(old_retirement.finish(env).is_err());
        let release = retirement
            .into_inner()
            .unwrap()
            .expect("actual cleanup must remain counted");
        drop(work);
        let pause = match pause.try_wait() {
            Err(pause) => pause,
            Ok(_) => panic!("cleanup must span canonical retirement"),
        };
        assert_eq!(
            control.retired().unwrap().finish(env).unwrap().phase,
            Phase::Cancelled
        );
        assert!(
            !staging.directory.join("auth.json").exists(),
            "retirement must not materialize C1"
        );
        let after_files: std::collections::BTreeSet<_> = std::fs::read_dir(&staging.directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(before_files, after_files);
        assert_eq!(
            codex_grant_owner::recover(env, &request, &reference)
                .unwrap()
                .reference(),
            reference
        );
        drop(release);
        assert!(
            pause.try_wait().is_ok(),
            "actual cleanup completion must drain"
        );
    });
}

#[test]
fn failed_native_claim_write_cannot_retire_same_counter_new_occurrence() {
    codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
        let [(request, _, fresh, _), _] = rows;
        let lifetime = account_work::Lifetime::default();
        retire_original(env, &request, &lifetime);
        let deadline =
            super::super::wake_deadline::WakeDeadline::after(super::super::login_drive::HANG);
        let before = store
            .peek_account_records(roster, env.now)
            .unwrap()
            .logins
            .to_json();
        let (first, first_control) = owned_work(request.clone(), lifetime.enter().unwrap());
        let candidate =
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).unwrap();
        let resources = codex_grant_owner::admit_native_recovery_publication(
            env,
            store,
            candidate,
            &fresh,
            &first.scope,
            deadline,
        )
        .unwrap();
        let held = store.acquire_bounded().unwrap().unwrap();
        let mut failed_receipt = None;
        let result = codex_native_row::with_current_row(
            env,
            (store, &held),
            roster,
            env.now,
            &request,
            &fresh,
            |current| {
                store.transact_roster_logins_held(
                    &held,
                    roster,
                    env.now,
                    &mut None,
                    |raw, state, logins| {
                        current.require_edit_image(raw, state, logins)?;
                        resources.check_store(store)?;
                        let receipt = logins
                            .enrollment
                            .claim_native_publication_recovery(&resources)?;
                        admission(&first).native_recovery(&receipt)?;
                        failed_receipt = Some(receipt);
                        Err::<Vec<accounts::Event>, String>(
                            "injected failure before canonical write".into(),
                        )
                    },
                )
            },
        );
        assert!(result.is_err());
        drop(held);
        assert_eq!(
            store
                .peek_account_records(roster, env.now)
                .unwrap()
                .logins
                .to_json(),
            before
        );
        drop(resources);
        drop(first);
        let failed_receipt = failed_receipt.unwrap();
        assert_eq!(failed_receipt.generation(), 1);
        let foreign_request = Request {
            id: "f".repeat(32),
            label: "foreign".into(),
            ..request.clone()
        };
        let (foreign_work, _) = owned_work(foreign_request, lifetime.enter().unwrap());
        assert!(
            admission(&foreign_work)
                .native_recovery(&failed_receipt)
                .is_err(),
            "a foreign Work cannot adopt the occurrence"
        );
        assert!(foreign_work.recovery.lock().unwrap().is_none());
        drop(foreign_work);
        let stale = first_control.retired().unwrap();
        let (second, _) = owned_work(request.clone(), lifetime.enter().unwrap());
        let candidate =
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).unwrap();
        let resources = codex_grant_owner::admit_native_recovery_publication(
            env,
            store,
            candidate,
            &fresh,
            &second.scope,
            deadline,
        )
        .unwrap();
        assert_ne!(failed_receipt.nonce(), Some(resources.occurrence()));
        let turn = codex_native_row::claim_retired_recovery(
            env,
            (store, roster),
            resources,
            &fresh,
            &admission(&second),
        )
        .unwrap();
        assert_eq!(
            turn.admission().claim(),
            1,
            "the failed write did not consume a canonical generation"
        );
        let records = store.peek_account_records(roster, env.now).unwrap();
        assert!(
            records
                .logins
                .enrollment
                .require_native_publication_recovery(&request, &failed_receipt)
                .is_err()
        );
        records
            .logins
            .enrollment
            .require_native_publication_recovery(&request, turn.admission().ownership())
            .unwrap();
        assert!(
            stale.finish(env).is_err(),
            "numeric counter reuse is not ownership"
        );
        assert_eq!(
            store.peek_account_records(roster, env.now).unwrap().logins,
            records.logins
        );
    });
}

#[test]
fn legacy_recovery_claim_encoding_has_no_native_occurrence() {
    let legacy = RecoveryClaim {
        generation: 1,
        retired: false,
        kind: None,
        occurrence: None,
    };
    assert_eq!(
        aterm_json::to_string(&legacy).unwrap(),
        "{\"generation\":1,\"retired\":false}"
    );
}

#[test]
fn same_request_receipt_cannot_move_to_another_live_scope() {
    codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
        let [(request, _, fresh, _), _] = rows;
        let lifetime = account_work::Lifetime::default();
        retire_original(env, &request, &lifetime);
        let deadline =
            super::super::wake_deadline::WakeDeadline::after(super::super::login_drive::HANG);
        let (first, first_control) = owned_work(request.clone(), lifetime.enter().unwrap());
        let candidate =
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).unwrap();
        let resources = codex_grant_owner::admit_native_recovery_publication(
            env,
            store,
            candidate,
            &fresh,
            &first.scope,
            deadline,
        )
        .unwrap();
        let turn = codex_native_row::claim_retired_recovery(
            env,
            (store, roster),
            resources,
            &fresh,
            &admission(&first),
        )
        .unwrap();
        let receipt = first.recovery.lock().unwrap().as_ref().unwrap().clone();
        let (second, second_control) = owned_work(request.clone(), lifetime.enter().unwrap());
        assert!(
            admission(&second).native_recovery(&receipt).is_err(),
            "same request is not the same operation Scope"
        );
        assert!(second.recovery.lock().unwrap().is_none());
        drop(second);
        assert!(second_control.retired().is_none());
        drop(first);
        assert!(
            !first_control.work_retired(),
            "actual resource Scope clone must remain live"
        );
        assert!(first_control.retired().is_none());
        let records = store.peek_account_records(roster, env.now).unwrap();
        records
            .logins
            .enrollment
            .require_native_publication_recovery(&request, &receipt)
            .unwrap();
        assert!(!records.logins.enrollment.is_retired(&request).unwrap());
        drop(turn);
        assert!(
            first_control.work_retired(),
            "weak receipt identity must not retain the Scope"
        );
        first_control.retired().unwrap().finish(env).unwrap();
        assert!(
            store
                .peek_account_records(roster, env.now)
                .unwrap()
                .logins
                .enrollment
                .is_retired(&request)
                .unwrap()
        );
    });
}

#[test]
fn native_retirement_ignores_changed_policy_roster_and_missing_stage() {
    for case in ["held", "removed", "malformed", "missing-stage"] {
        codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
            let [(request, staging, fresh, _), _] = rows;
            let lifetime = account_work::Lifetime::default();
            retire_original(env, &request, &lifetime);
            let deadline =
                super::super::wake_deadline::WakeDeadline::after(super::super::login_drive::HANG);
            let (original, original_control) =
                owned_work(request.clone(), lifetime.enter().unwrap());
            let candidate =
                codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh)
                    .unwrap();
            let resources = codex_grant_owner::admit_native_recovery_publication(
                env,
                store,
                candidate,
                &fresh,
                &original.scope,
                deadline,
            )
            .unwrap();
            let turn = codex_native_row::claim_retired_recovery(
                env,
                (store, roster),
                resources,
                &fresh,
                &admission(&original),
            )
            .unwrap();
            match case {
                "held" => {
                    let mut raw = accounts::Roster::load_for(accounts::Cli::Codex, roster).unwrap();
                    let row = raw
                        .rows
                        .iter_mut()
                        .find(|row| row.label == request.label)
                        .unwrap();
                    row.email = Some("changed@example.test".into());
                    row.held_by = Some("pool".into());
                    std::fs::write(
                        roster,
                        raw.rows
                            .iter()
                            .map(|row| accounts::row_text(row).unwrap())
                            .collect::<String>(),
                    )
                    .unwrap();
                }
                "removed" => std::fs::write(roster, "# current rows were removed\n").unwrap(),
                "malformed" => std::fs::write(roster, "not [valid TOML").unwrap(),
                "missing-stage" => {
                    std::fs::remove_dir_all(&staging.directory).unwrap();
                    std::fs::remove_file(roster).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                store
                    .load_logins(env.now)
                    .unwrap()
                    .0
                    .enrollment
                    .native_cancellation(&request)
                    .unwrap()
            );
            assert_eq!(
                original_control.cancel(env).unwrap().phase,
                Phase::CancelRequested
            );
            assert!(
                !original_control.work_retired(),
                "actual resources still own their Scope"
            );
            drop(turn);
            drop(original);
            let before_roster = std::fs::read(roster).ok();
            let (before, _) = store.load_logins(env.now).unwrap();
            let (work, control) = owned_work(request.clone(), lifetime.enter().unwrap());
            let retirement = std::sync::Mutex::new(None);
            let context = NativeContext {
                env,
                runtime: &work.native,
                scope: &work.scope,
                retirement: &retirement,
            };
            let status = cli::enroll_codex_operation_custody(
                env,
                &LocalOnly(env.now),
                &request,
                Some(&admission(&work)),
                Some(&context),
            )
            .unwrap();
            assert_eq!(status.phase, Phase::CancelRequested, "{case}");
            let release = retirement.into_inner().unwrap().unwrap();
            drop(work);
            assert_eq!(
                control.retired().unwrap().finish(env).unwrap().phase,
                Phase::Cancelled,
                "{case}"
            );
            let (after, _) = store.load_logins(env.now).unwrap();
            assert_eq!(
                after.labels, before.labels,
                "retirement must not change binding policy: {case}"
            );
            assert_eq!(after.generation_clock, before.generation_clock);
            assert!(
                after
                    .enrollment
                    .0
                    .get(&request.id)
                    .unwrap()
                    .owner_witness
                    .is_none()
            );
            assert_eq!(std::fs::read(roster).ok(), before_roster);
            assert!(
                !staging.directory.join("auth.json").exists(),
                "no credential materialization: {case}"
            );
            if case == "missing-stage" {
                assert!(
                    !staging.directory.exists(),
                    "cleanup must not recreate an old stage"
                );
            }
            assert_eq!(
                codex_grant_owner::recover(env, &request, &fresh.reference())
                    .unwrap()
                    .reference(),
                fresh.reference()
            );
            drop(release);
        });
    }
}

#[test]
fn live_native_cancellation_and_final_retirement_do_not_parse_roster() {
    codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
        let [(request, staging, _, _), _] = rows;
        let lifetime = account_work::Lifetime::default();
        let (work, control) = owned_work(request.clone(), lifetime.enter().unwrap());
        // Synthetic zero-external-resource original admission, actual counted Scope.
        work.admitted
            .store(true, std::sync::atomic::Ordering::Release);
        std::fs::write(roster, "not [valid TOML").unwrap();
        let (before, _) = store.load_logins(env.now).unwrap();
        assert!(before.enrollment.native_cancellation(&request).unwrap());
        assert!(!before.enrollment.native_recovery_pending(&request).unwrap());
        assert_eq!(control.cancel(env).unwrap().phase, Phase::CancelRequested);
        assert!(
            control.retired().is_none(),
            "live original Scope cannot be declared retired"
        );
        drop(work);
        assert_eq!(
            control.retired().unwrap().finish(env).unwrap().phase,
            Phase::Cancelled
        );
        let (after, _) = store.load_logins(env.now).unwrap();
        assert_eq!(after.labels, before.labels);
        assert_eq!(after.generation_clock, before.generation_clock);
        assert!(!staging.directory.join("auth.json").exists());
        assert_eq!(std::fs::read_to_string(roster).unwrap(), "not [valid TOML");
    });
}

#[test]
fn pending_record_publication_is_repaired_before_native_cancel_classification() {
    codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
        let [(request, _, _, _), _] = rows;
        let lifetime = account_work::Lifetime::default();
        let (work, control) = owned_work(request.clone(), lifetime.enter().unwrap());
        // Zero external resources in this synthetic original, real Scope truth.
        work.admitted
            .store(true, std::sync::atomic::Ordering::Release);
        let wake = store.wake_path();
        match std::fs::remove_file(&wake) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("{error}"),
        }
        std::fs::create_dir(&wake).unwrap();
        let roster_before = std::fs::read(roster).unwrap();
        let prepared = store.observe_admitted_workflow_for_test(
            roster,
            &request.label,
            env.now,
            |_, state, _| {
                state.previous = Some(request.label.clone());
                vec![accounts::Event::new(
                    env.now,
                    accounts::Kind::Use,
                    &request.label,
                )]
            },
        );
        assert!(prepared.is_err());
        assert!(
            store.load_logins(env.now).is_err(),
            "a pending receipt is not a native classification"
        );
        std::fs::remove_dir(&wake).unwrap();
        assert_eq!(control.cancel(env).unwrap().phase, Phase::CancelRequested);
        assert!(store.load_logins(env.now).is_ok());
        assert_eq!(
            store.load(env.now).unwrap().0.previous.as_deref(),
            Some(request.label.as_str())
        );
        assert_eq!(std::fs::read(roster).unwrap(), roster_before);
        assert!(control.retired().is_none());
        drop(work);
        assert_eq!(
            control.retired().unwrap().finish(env).unwrap().phase,
            Phase::Cancelled
        );
    });
}

#[test]
fn cold_cancelled_native_release_runs_while_ordinary_admission_is_paused() {
    codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
        let [(request, staging, fresh, _), _] = rows;
        let lifetime = account_work::Lifetime::default();
        retire_original(env, &request, &lifetime);
        let (original, original_control) = owned_work(request.clone(), lifetime.enter().unwrap());
        let candidate =
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).unwrap();
        let resources = codex_grant_owner::admit_native_recovery_publication(
            env,
            store,
            candidate,
            &fresh,
            &original.scope,
            super::super::wake_deadline::WakeDeadline::after(super::super::login_drive::HANG),
        )
        .unwrap();
        let turn = codex_native_row::claim_retired_recovery(
            env,
            (store, roster),
            resources,
            &fresh,
            &admission(&original),
        )
        .unwrap();
        assert_eq!(
            original_control.cancel(env).unwrap().phase,
            Phase::CancelRequested
        );
        drop(turn);
        drop(original); // Simulated sender loss, no canonical retirement write.
        let before = store.load_logins(env.now).unwrap().0;
        let sealed = account_work::Lifetime::default();
        let sealed_pause = sealed.pause().unwrap();
        let sealed_drained = sealed_pause
            .try_wait()
            .unwrap_or_else(|_| panic!("empty lifetime seals"));
        assert!(reserve_cancelled_retirement(env, &request, &sealed).is_err());
        assert_eq!(store.load_logins(env.now).unwrap().0, before);
        drop(sealed_drained);
        let pause = lifetime.pause().unwrap();
        assert!(lifetime.enter().is_none());
        let (work, control) = reserve_cancelled_retirement(env, &request, &lifetime).unwrap();
        let (stale_work, stale_control) =
            reserve_cancelled_retirement(env, &request, &lifetime).unwrap();
        assert!(control.owned_by(&lifetime));
        assert!(!control.work_retired());
        assert_eq!(work.run().unwrap().phase, Phase::Cancelled);
        assert!(control.work_retired());
        let completed = store.load_logins(env.now).unwrap().0;
        assert!(stale_work.run().is_err());
        assert!(stale_control.work_retired());
        assert!(
            stale_control.retired().is_none(),
            "same request does not transfer cleanup receipt"
        );
        assert_eq!(store.load_logins(env.now).unwrap().0, completed);
        let after = store.load_logins(env.now).unwrap().0;
        assert_eq!(after.labels, before.labels);
        assert_eq!(after.generation_clock, before.generation_clock);
        assert_eq!(
            codex_grant_owner::recover(env, &request, &fresh.reference())
                .unwrap()
                .reference(),
            fresh.reference()
        );
        assert!(!staging.directory.join("auth.json").exists());
        let drained = pause
            .try_wait()
            .unwrap_or_else(|_| panic!("release cleanup must really retire"));
        assert!(reserve_cancelled_retirement(env, &request, &lifetime).is_err());
        drop(drained);
    });
}

#[test]
fn pre_attachment_native_response_recovers_from_producer_or_actual_claim_without_exchange() {
    codex_grant_owner::with_unattached_native_rows_fixture(|env, store, roster, rows| {
        let lifetime = account_work::Lifetime::default();
        for (index, (request, staging, fresh, _)) in rows.into_iter().enumerate() {
            let before = store.load_logins(env.now).unwrap().0;
            assert!(before.enrollment.checkpoint_reference(&request).is_err());
            assert!(
                before
                    .enrollment
                    .native_recovery_reference(&request)
                    .is_err()
            );
            retire_original(env, &request, &lifetime);
            let retired = store.load_logins(env.now).unwrap().0;
            assert_eq!(
                retired
                    .enrollment
                    .native_recovery_reference(&request)
                    .unwrap(),
                None
            );
            assert!(retired.enrollment.checkpoint_reference(&request).is_err());
            assert_eq!(
                codex_grant_owner::recover_pending(env, &request)
                    .unwrap()
                    .reference(),
                fresh.reference()
            );
            if index == 1 {
                let (first, _) = owned_work(request.clone(), lifetime.enter().unwrap());
                let candidate =
                    codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh)
                        .unwrap();
                let resources = codex_grant_owner::admit_native_recovery_publication(
                    env,
                    store,
                    candidate,
                    &fresh,
                    &first.scope,
                    super::super::wake_deadline::WakeDeadline::after(
                        super::super::login_drive::HANG,
                    ),
                )
                .unwrap();
                let turn = codex_native_row::claim_retired_recovery(
                    env,
                    (store, roster),
                    resources,
                    &fresh,
                    &admission(&first),
                )
                .unwrap();
                drop(turn);
                drop(first); // Real resource retirement, crash before attach/retirement publication.
                let claimed = store.load_logins(env.now).unwrap().0;
                assert_eq!(
                    claimed
                        .enrollment
                        .native_recovery_reference(&request)
                        .unwrap(),
                    Some(fresh.reference())
                );
                assert!(claimed.enrollment.checkpoint_reference(&request).is_err());
            }
            let (work, _) = owned_work(request.clone(), lifetime.enter().unwrap());
            let retirement = std::sync::Mutex::new(None);
            let context = NativeContext {
                env,
                runtime: &work.native,
                scope: &work.scope,
                retirement: &retirement,
            };
            let status = cli::enroll_codex_operation_custody(
                env,
                &LocalOnly(env.now),
                &request,
                Some(&admission(&work)),
                Some(&context),
            )
            .unwrap();
            assert_eq!(status.phase, Phase::Applied);
            let records = store.peek_account_records(roster, env.now).unwrap();
            let binding = records
                .logins
                .enrollment
                .published_grant(&request.label, status.generation.unwrap())
                .unwrap();
            assert_eq!(binding.reference, fresh.reference());
            assert_eq!(
                records
                    .logins
                    .enrollment
                    .owner_witness(&binding)
                    .unwrap()
                    .unwrap()
                    .revision
                    .generation,
                1
            );
            assert!(staging.directory.join("auth.json").exists());
        }
    });
}

#[test]
fn pre_attachment_native_cancel_uses_actual_claim_reference_without_materializing() {
    codex_grant_owner::with_unattached_native_rows_fixture(|env, store, roster, rows| {
        let [(request, staging, fresh, _), _] = rows;
        let lifetime = account_work::Lifetime::default();
        retire_original(env, &request, &lifetime);
        let (first, control) = owned_work(request.clone(), lifetime.enter().unwrap());
        let candidate =
            codex_native_row::recovery_candidate(env, (store, roster), &request, &fresh).unwrap();
        let resources = codex_grant_owner::admit_native_recovery_publication(
            env,
            store,
            candidate,
            &fresh,
            &first.scope,
            super::super::wake_deadline::WakeDeadline::after(super::super::login_drive::HANG),
        )
        .unwrap();
        let turn = codex_native_row::claim_retired_recovery(
            env,
            (store, roster),
            resources,
            &fresh,
            &admission(&first),
        )
        .unwrap();
        assert_eq!(control.cancel(env).unwrap().phase, Phase::CancelRequested);
        drop(turn);
        drop(first);
        let before = store.load_logins(env.now).unwrap().0;
        assert_eq!(
            before
                .enrollment
                .native_retirement_reference(&request)
                .unwrap(),
            Some(fresh.reference())
        );
        let pause = lifetime.pause().unwrap();
        let (cleanup, _) = reserve_cancelled_retirement(env, &request, &lifetime).unwrap();
        assert_eq!(cleanup.run().unwrap().phase, Phase::Cancelled);
        let after = store.load_logins(env.now).unwrap().0;
        assert_eq!(before.labels, after.labels);
        assert_eq!(before.generation_clock, after.generation_clock);
        assert!(!staging.directory.join("auth.json").exists());
        assert_eq!(
            codex_grant_owner::recover_pending(env, &request)
                .unwrap()
                .reference(),
            fresh.reference()
        );
        assert!(pause.try_wait().is_ok());
    });
}

#[test]
fn current_global_code_owner_recovers_actual_sender_response_across_publication_cuts() {
    use super::super::{codex_app_login, native_enrollment_admission};
    for (cut, predecessor) in [
        (codex_app_login::EnrollmentCut::BeforeSeal, false),
        (codex_app_login::EnrollmentCut::BeforeSeal, true),
        (codex_app_login::EnrollmentCut::AfterSeal, false),
        (codex_app_login::EnrollmentCut::AfterSeal, true),
        (codex_app_login::EnrollmentCut::AfterAuthWrite, true),
    ] {
        codex_grant_owner::with_global_code_fixture(
            |env, request, auth, turn, prepared, artifact| {
                let lifetime = account_work::Lifetime::default();
                let scope = lifetime.enter().unwrap();
                let (work, control) = owned_work(request.clone(), scope.clone());
                let outcome = codex_app_login::tests::current_global_code_fixture(
                    &artifact,
                    prepared,
                    &scope,
                    Some(cut),
                    true,
                    predecessor,
                    env,
                );
                let codex_grant_owner::CaptureOutcome::Verified(retained) = &outcome else {
                    panic!("actual supported-bundle response must retain a verified issuer");
                };
                let original_reference = retained.reference();
                work.fixture_capture_after_native_admission(env, outcome);
                let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
                let (captured, warning) = store.load_logins(env.now).unwrap();
                assert!(warning.is_none());
                assert!(
                    captured
                        .enrollment
                        .requires_global_catalogue(request)
                        .unwrap()
                );
                assert_eq!(
                    captured
                        .enrollment
                        .catalogue_witness(request)
                        .unwrap()
                        .unwrap()
                        .phase,
                    CataloguePhase::Received
                );
                assert!(
                    captured
                        .enrollment
                        .catalogue_staging(request)
                        .unwrap()
                        .native_row
                        .is_some()
                );
                // Provider response is durable before the real exchange exclusion
                // releases. No artefact bytes or historical admission bit grant it.
                assert!(native_enrollment_admission::RefreshExchangeLease::for_env(env).is_ok());
                drop(turn);
                drop(work);
                drop(scope);
                control.settle_custody(env).unwrap();
                control.retired().unwrap().finish(env).unwrap();
                let (recovery, _) = owned_work(request.clone(), lifetime.enter().unwrap());
                let retirement = std::sync::Mutex::new(None);
                let context = NativeContext {
                    env,
                    runtime: &recovery.native,
                    scope: &recovery.scope,
                    retirement: &retirement,
                };
                let status = cli::enroll_codex_operation_custody(
                    env,
                    &LocalOnly(env.now),
                    request,
                    Some(&admission(&recovery)),
                    Some(&context),
                )
                .unwrap();
                assert_eq!(status.phase, Phase::Applied);
                let (after, warning) = store.load_logins(env.now).unwrap();
                assert!(warning.is_none());
                let binding = after
                    .enrollment
                    .published_grant(&request.label, status.generation.unwrap())
                    .unwrap();
                assert_eq!(binding.reference, original_reference);
                let current = after.enrollment.owner_witness(&binding).unwrap().unwrap();
                assert_eq!(current.revision.generation, 1);
                assert!(current.pending.is_none());
                assert_eq!(
                    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
                        std::fs::read(auth).unwrap()
                    )),
                    current.revision.digest
                );
                assert_eq!(
                    codex_grant_owner::recover_pending(env, request)
                        .unwrap()
                        .reference(),
                    original_reference
                );
                let before_replay = after.to_json();
                let replay = cli::enroll_codex_operation_custody(
                    env,
                    &LocalOnly(env.now),
                    request,
                    Some(&admission(&recovery)),
                    Some(&context),
                )
                .unwrap();
                assert_eq!(replay, status);
                assert_eq!(
                    store.load_logins(env.now).unwrap().0.to_json(),
                    before_replay
                );
            },
        );
    }
}

#[test]
fn current_global_code_sender_override_does_not_reclassify_unsupported_realm() {
    use super::super::codex_app_login;
    codex_grant_owner::with_global_code_fixture(|env, request, _auth, turn, prepared, artifact| {
        let lifetime = account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let (work, control) = owned_work(request.clone(), scope.clone());
        let outcome = codex_app_login::tests::current_global_code_fixture(
            &artifact,
            prepared,
            &scope,
            Some(codex_app_login::EnrollmentCut::BeforeSeal),
            false,
            false,
            env,
        );
        assert!(matches!(
            &outcome,
            codex_grant_owner::CaptureOutcome::Verified(_)
        ));
        work.fixture_capture_after_native_admission(env, outcome);
        let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
        let (captured, warning) = store.load_logins(env.now).unwrap();
        assert!(warning.is_none());
        assert_eq!(
            captured
                .enrollment
                .catalogue_witness(request)
                .unwrap()
                .unwrap()
                .phase,
            CataloguePhase::Quarantined
        );
        drop(turn);
        drop(work);
        drop(scope);
        control.settle_custody(env).unwrap();
        control.retired().unwrap().finish(env).unwrap();
        let before = store.load_logins(env.now).unwrap().0;
        let (recovery, _) = owned_work(request.clone(), lifetime.enter().unwrap());
        let retirement = std::sync::Mutex::new(None);
        let context = NativeContext {
            env,
            runtime: &recovery.native,
            scope: &recovery.scope,
            retirement: &retirement,
        };
        let _ = cli::enroll_codex_operation_custody(
            env,
            &LocalOnly(env.now),
            request,
            Some(&admission(&recovery)),
            Some(&context),
        );
        let after = store.load_logins(env.now).unwrap().0;
        assert_eq!(after.generation_clock, before.generation_clock);
        assert!(
            after
                .enrollment
                .published_grant(&request.label, after.generation_clock)
                .is_none()
        );
        assert!(codex_grant_owner::recover_pending(env, request).is_ok());
    });
}
