// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
fn exchange_file(env: &Env) -> std::fs::File {
    let namespace = crate::harness::codex_source_gate::existing_namespace_for_home(
        env.home.as_deref().unwrap(),
    )
    .unwrap();
    private_file::open_private_rw(namespace.directory().unwrap(), "native-code-exchange.lock")
        .unwrap()
}

#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "test the actual kernel exclusion used by native refresh"
)]
fn exchange_is_exclusive(file: &std::fs::File) -> bool {
    match file.try_lock() {
        Ok(()) => true,
        Err(std::fs::TryLockError::WouldBlock) => false,
        Err(std::fs::TryLockError::Error(error)) => panic!("kernel lock failed: {error}"),
    }
}

#[cfg(unix)]
#[test]
fn native_exchange_exclusion_precedes_owner_work_and_releases_after_completion() {
    // Real kernel locks only; this fixture grants no credential or HTTP authority.
    let fixture = super::super::super::maintenance_tests::fixture();
    let lease = RefreshWorkLease::for_env(&fixture.env).unwrap();
    let file = exchange_file(&fixture.env);
    assert!(!exchange_is_exclusive(&file));
    drop(lease);
    assert!(exchange_is_exclusive(&file));
    let mut entered = false;
    let result = with_refresh_work(&fixture.env, |_| {
        entered = true;
        Ok(())
    });
    assert!(matches!(result, Err(OwnerRefreshError::Unavailable)));
    assert!(!entered, "exchange contention must precede owner work");
    let retained = file.try_clone().unwrap();
    // Release this test-owned probe explicitly: a forked child's duplicate
    // must not keep our deliberate blocker held until its exec.
    file.unlock().unwrap();
    drop(file);
    with_refresh_work(&fixture.env, |work| {
        work.current().unwrap();
        assert!(!exchange_is_exclusive(&exchange_file(&fixture.env)));
        Ok(())
    })
    .unwrap();
    drop(retained);
    assert!(exchange_is_exclusive(&exchange_file(&fixture.env)));
}

#[cfg(unix)]
#[test]
fn legacy_signin_excludes_refresh_before_owner_work() {
    let fixture = super::super::super::maintenance_tests::fixture();
    let store = fixture
        .env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    let signin = store.signin_now().unwrap().unwrap();
    let mut entered = false;
    let result = with_refresh_work(&fixture.env, |_| {
        entered = true;
        Ok(())
    });
    assert!(matches!(result, Err(OwnerRefreshError::Unavailable)));
    assert!(
        !entered,
        "legacy sign-in must exclude refresh before owner work"
    );
    let publication = crate::harness::codex_source_gate::PublicationLock::for_home(
        fixture.env.home.as_deref().unwrap(),
    )
    .expect("failed refresh admission must release its source lease");
    drop(publication);
    drop(signin);
    with_refresh_work(&fixture.env, |work| {
        work.current().unwrap();
        Ok(())
    })
    .unwrap();
}

#[cfg(unix)]
#[test]
fn uncertain_retention_and_continuation_keep_the_original_exchange_lease() {
    // Force an actual encrypted-store failure. This tests custody plumbing,
    // not a provider response or a qualified process/source census.
    let fixture = super::super::super::maintenance_tests::fixture();
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let claimed =
        super::super::super::with_maintenance_owner(&fixture.env, &fixture.binding, &scope, |_| {
            Claimed::open(&fixture.env, &fixture.binding, false)?.ok_or("journal absent".into())
        })
        .unwrap();
    let directory = claimed.directory.clone();
    let held_directory = directory.with_extension("retention-test-held");
    let result = with_refresh_work::<()>(&fixture.env, |_| {
        std::fs::rename(&directory, &held_directory).unwrap();
        let Err(retention) = claimed.persist() else {
            panic!("moved store must refuse the write");
        };
        Err(OwnerRefreshError::Retaining(retention))
    });
    let Err(OwnerRefreshError::Retaining(retention)) = result else {
        panic!("uncertain write lost its owned retention");
    };
    assert!(!exchange_is_exclusive(&exchange_file(&fixture.env)));
    std::fs::rename(&held_directory, &directory).unwrap();
    let continuation = retention.retry().unwrap();
    assert!(!exchange_is_exclusive(&exchange_file(&fixture.env)));
    continuation.reconcile(&fixture.env, &scope).unwrap();
    assert!(exchange_is_exclusive(&exchange_file(&fixture.env)));
}

#[cfg(unix)]
#[test]
fn completed_raw_retains_original_scope_through_end_and_paused_no_caller_settlement() {
    completed_raw_after_end(false);
}

#[cfg(unix)]
#[test]
fn explicit_hold_defers_durable_raw_without_pinning_ended_source_scope() {
    completed_raw_after_end(true);
}

#[cfg(unix)]
fn completed_raw_after_end(held: bool) {
    let fixture = super::super::super::maintenance_tests::fixture();
    completed_raw_in(&fixture.env, &fixture.binding, &fixture.auth, held, false);
}

#[cfg(unix)]
#[test]
fn global_held_raw_deferral_retires_actual_principal_and_token_resources() {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"b".repeat(64));
    fixture.retire_originals().unwrap();
    let status = fixture.publish_first().unwrap();
    assert_eq!(
        status.phase,
        crate::harness::codex_enrollment::Phase::Applied
    );
    let env = fixture.env();
    let store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    let (logins, warning) = store.load_logins(env.now).unwrap();
    assert!(warning.is_none());
    let request = &fixture.requests()[0];
    let binding = logins
        .enrollment
        .published_grant(&request.label, status.generation.unwrap())
        .unwrap();
    let auth = logins
        .enrollment
        .catalogue_staging(request)
        .unwrap()
        .directory
        .join("auth.json");
    completed_raw_in(env, &binding, &auth, true, true);
}

#[cfg(unix)]
fn completed_raw_in(env: &Env, binding: &GrantBinding, auth: &Path, held: bool, global: bool) {
    let lifetime = crate::harness::account_work::Lifetime::default();
    let source_use = lifetime.enter().unwrap();
    let worker = lifetime.enter().unwrap();
    let source_status = source_use.cancellation();
    inject_received_response(env, binding, &source_use);
    let claimed = super::super::super::with_maintenance_owner(env, binding, &source_use, |_| {
        Claimed::open(env, binding, false)?.ok_or("owned response missing".into())
    })
    .unwrap();
    let resource_probe = global.then(|| {
        super::super::super::response_resource_identity(
            &binding.request,
            &codex_app_login::renewal_identity(auth.parent().unwrap()).unwrap(),
            &std::fs::read(auth).unwrap(),
        )
        .unwrap()
    });
    let directory = claimed.directory.clone();
    let displaced = directory.with_extension("counted-raw-held");
    let failed = with_refresh_work_scoped::<()>(env, &source_use, None, |work| {
        work.provider_current().unwrap();
        let lineage = work.reconcile_lineage(env, binding).unwrap();
        assert_eq!(lineage.is_some(), global);
        drop(lineage); // The Work retains the real resource claim.
        if let Some(proof) = &resource_probe {
            let namespace = crate::harness::codex_source_gate::existing_namespace_for_home(
                env.home.as_deref().unwrap(),
            )
            .unwrap();
            assert!(
                crate::harness::native_enrollment_admission::RefreshResourceTurn::try_acquire(
                    namespace, proof
                )
                .unwrap()
                .is_none()
            );
        }
        std::fs::rename(&directory, &displaced).unwrap();
        let Err(retention) = claimed.persist() else {
            panic!("fixture write must fail");
        };
        Err(OwnerRefreshError::Retaining(retention))
    });
    let Err(OwnerRefreshError::Retaining(retention)) = failed else {
        panic!("owned raw response was erased");
    };
    source_status.cancel();
    drop(source_use);
    assert!(
        !source_status.retired(),
        "End cannot retire the actual source operation while raw custody remains"
    );
    let pause = lifetime.pause().unwrap();
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("raw and worker operations still count"),
    };
    let work = retention.work_lease.as_ref().unwrap();
    assert!(
        work.provider_current().is_err(),
        "revoked source cannot mint a new provider request"
    );
    work.current().unwrap();
    std::fs::rename(&displaced, &directory).unwrap();
    let continuation = retention.retry().unwrap();
    assert!(!source_status.retired());
    // The caller PID/Slot is never consulted. The genuine captured source
    // scope, rather than the worker's separate operation, counts promotion.
    let roster = env
        .accounts_roster_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    let original_roster = std::fs::read(&roster).unwrap();
    let original_auth = std::fs::read(auth).unwrap();
    if held {
        let mut held_roster = crate::harness::accounts::Roster::parse_for(
            crate::harness::accounts::Cli::Codex,
            std::str::from_utf8(&original_roster).unwrap(),
        )
        .unwrap();
        held_roster
            .rows
            .iter_mut()
            .find(|row| row.label == binding.request.label)
            .unwrap()
            .held_by = Some("explicit-security-hold".into());
        let held_text = held_roster
            .rows
            .iter()
            .map(|row| crate::harness::accounts::row_text(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&roster, held_text).unwrap();
        assert!(matches!(
            continuation.reconcile(env, &worker),
            Err(OwnerRefreshError::Unavailable)
        ));
        assert_eq!(std::fs::read(auth).unwrap(), original_auth);
        let store = env
            .accounts_store_for(crate::harness::accounts::Cli::Codex)
            .unwrap();
        let (logins, warning) = store.load_logins(env.now).unwrap();
        assert!(warning.is_none());
        let witness = logins.enrollment.owner_witness(binding).unwrap().unwrap();
        assert_eq!(witness.revision.generation, 1);
        assert!(witness.pending.is_some());
        let claimed = Claimed::open(env, binding, false).unwrap().unwrap();
        assert!(matches!(
            claimed
                .journal
                .as_ref()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .phase,
            Phase::Responded
        ));
        drop(claimed);
    } else {
        continuation.reconcile(env, &worker).unwrap();
    }
    assert!(source_status.retired());
    assert!(exchange_is_exclusive(&exchange_file(env)));
    let publication =
        crate::harness::codex_source_gate::PublicationLock::for_home(env.home.as_deref().unwrap())
            .expect("settled or durably deferred response must release source exclusion");
    drop(publication);
    if let Some(proof) = &resource_probe {
        let namespace = crate::harness::codex_source_gate::existing_namespace_for_home(
            env.home.as_deref().unwrap(),
        )
        .unwrap();
        let released =
            crate::harness::native_enrollment_admission::RefreshResourceTurn::try_acquire(
                namespace, proof,
            )
            .unwrap();
        assert!(
            released.is_some(),
            "no principal/actual-token claim may outlive the retired source count"
        );
        drop(released);
    }
    assert!(worker.cancelled());
    drop(worker);
    assert!(pause.try_wait().is_ok());
    if held {
        // Explicit hold never grants delivery or cache mutation. A later
        // actual counted maintenance operation may settle the same durable
        // response after the hold is removed; no second HTTP send occurs.
        std::fs::write(&roster, original_roster).unwrap();
        let maintenance_lifetime = crate::harness::account_work::Lifetime::default();
        let maintenance_scope = maintenance_lifetime.enter().unwrap();
        reconcile_retained(env, binding, &maintenance_scope).unwrap();
    }
    let (logins, warning) = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap()
        .load_logins(env.now)
        .unwrap();
    assert!(warning.is_none());
    assert_eq!(
        logins
            .enrollment
            .owner_witness(binding)
            .unwrap()
            .unwrap()
            .revision
            .generation,
        2
    );
}

#[cfg(unix)]
#[test]
fn confirmed_received_token_alias_is_durable_without_self_pinning_source_end() {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"b".repeat(64));
    fixture.retire_originals().unwrap();
    let applied_a = fixture.publish_first().unwrap();
    let env = fixture.env();
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let (work, _) =
        crate::harness::codex_enrollment::owned_work(fixture.requests()[1].clone(), scope.clone());
    let applied_b = fixture.run_recovery(work).unwrap();
    assert_eq!(
        applied_b.phase,
        crate::harness::codex_enrollment::Phase::Applied
    );
    let store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    let (logins, warning) = store.load_logins(env.now).unwrap();
    assert!(warning.is_none());
    let a = logins
        .enrollment
        .published_grant(&fixture.requests()[0].label, applied_a.generation.unwrap())
        .unwrap();
    let b = logins
        .enrollment
        .published_grant(&fixture.requests()[1].label, applied_b.generation.unwrap())
        .unwrap();
    fixture_promote_lineage_response(env, &a, &scope, "shared-actual-candidate-fixture");
    inject_lineage_response(env, &b, &scope, None, "shared-actual-candidate-fixture");
    let auth_b = logins
        .enrollment
        .catalogue_staging(&b.request)
        .unwrap()
        .directory
        .join("auth.json");
    let unchanged_b = std::fs::read(&auth_b).unwrap();
    let ended = scope.cancellation();
    ended.cancel();
    assert!(matches!(
        reconcile_retained(env, &b, &scope),
        Err(OwnerRefreshError::Unavailable)
    ));
    let (after, warning) = store.load_logins(env.now).unwrap();
    assert!(warning.is_none());
    let witness = after.enrollment.owner_witness(&b).unwrap().unwrap();
    assert_eq!(witness.revision.generation, 1);
    assert!(witness.pending.is_some());
    let claimed = Claimed::open(env, &b, false).unwrap().unwrap();
    let journal = claimed.journal.as_ref().unwrap();
    assert_eq!(journal.schema, 4);
    assert!(journal.completed.is_empty());
    let pending = journal.pending.as_ref().unwrap();
    assert!(pending.phase == Phase::Quarantined);
    assert!(pending.quarantine_reason == Some(QuarantineReason::KnownAlias));
    assert!(pending.response.is_some());
    drop(claimed);
    assert_eq!(std::fs::read(&auth_b).unwrap(), unchanged_b);
    drop(scope);
    assert!(
        ended.retired(),
        "durable refusal must not retain the ended source's count"
    );
    let next_lifetime = crate::harness::account_work::Lifetime::default();
    let next = next_lifetime.enter().unwrap();
    for binding in [&a, &b] {
        with_refresh_work_scoped(env, &next, None, |work| {
            assert!(
                work.send_lineage(env, binding).is_err(),
                "neither overlapping owner may reuse the shared token"
            );
            Ok(())
        })
        .unwrap();
    }
    // These are synthetic received-boundary bodies with real encrypted
    // journals/anchors/resources. No network or provider independence is
    // inferred from this adversarial catalogue control.
}

#[cfg(unix)]
fn recorded_auth(env: &Env, label: &str) -> crate::harness::accounts::Auth {
    let store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    let roster = env
        .accounts_roster_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    let records = store.read_account_records(Some(&roster), env.now).unwrap();
    let row = records.roster.row(label).unwrap();
    records.state.account(label).current_auth(row)
}

#[cfg(unix)]
fn record_contextual_auth(env: &Env, label: &str, word: crate::harness::accounts::Auth, at: i64) {
    let store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    let roster = env
        .accounts_roster_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    let records = store.read_account_records(Some(&roster), env.now).unwrap();
    let row = records.roster.row(label).unwrap();
    let context = store.capture_auth_context(row).unwrap();
    store
        .transact_logins(env.now, &mut None, |state, logins| {
            crate::harness::accounts::stage_auth(state, row, logins, context, word, at);
            Ok(Vec::new())
        })
        .unwrap();
    let (state, warning) = store.load(env.now).unwrap();
    assert!(warning.is_none());
    assert_eq!(state.account(label).auth, word);
    assert_eq!(state.account(label).auth_at, Some(at));
}

#[cfg(unix)]
fn replace_canonical_refresh(directory: &Path, replacement: &str) {
    let mut document: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(directory.join("auth.json")).unwrap()).unwrap();
    document
        .as_object_mut()
        .and_then(|document| document.get_mut("tokens"))
        .and_then(aterm_json::Value::as_object_mut)
        .unwrap()
        .insert(
            "refresh_token".into(),
            aterm_json::Value::String(replacement.into()),
        );
    let mut encoded = aterm_json::to_vec(&document).unwrap();
    encoded.push(b'\n');
    platform::write_private(directory, "auth.json", &encoded).unwrap();
}

/// A REFUSED REFRESH REJECTS ITS GRANT WITHOUT HOLDING OTHER GRANTS: the issuer's explicit
/// token refusal issued no successor, so reconciliation records `Rejected`, refuses another
/// send, and leaves the machine catalogue available. Once the unchanged credential outlives
/// the rotation-publication window, maintenance records its contextual account word SignedOut.
/// CONTROLS: before that deadline it does not; a bare gateway 401 and a transient 503 are
/// terminally quarantined for this principal but never signed out; replacing `auth.json` before
/// the deadline refuses settlement. Before either terminal classification, the answer blocks a
/// code exchange, while afterwards neither terminal grant does.
#[cfg(unix)]
#[test]
fn a_refused_refresh_rejects_its_grant_and_leaves_the_catalogue_current() {
    let cases: [(&str, u16, &[u8], bool, bool); 6] = [
        (
            "invalid grant",
            400,
            br#"{"error":"invalid_grant"}"#,
            true,
            false,
        ),
        (
            "expired refresh",
            401,
            br#"{"error":{"code":"refresh_token_expired"}}"#,
            true,
            false,
        ),
        (
            "current rejected token",
            400,
            br#"{"error":"invalid_grant"}"#,
            true,
            false,
        ),
        (
            "gateway 401",
            401,
            b"<html>gateway challenge</html>",
            false,
            false,
        ),
        (
            "temporary failure",
            503,
            br#"{"error":"temporarily_unavailable"}"#,
            false,
            false,
        ),
        (
            "credential replaced",
            400,
            br#"{"error":"invalid_grant"}"#,
            true,
            true,
        ),
    ];
    for (name, status, body, refused, replace_auth) in cases {
        let fixture =
            crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"b".repeat(64));
        fixture.retire_originals().unwrap();
        let applied_a = fixture.publish_first().unwrap();
        let env = fixture.env();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let (work, _) = crate::harness::codex_enrollment::owned_work(
            fixture.requests()[1].clone(),
            scope.clone(),
        );
        fixture.run_recovery(work).unwrap();
        let (logins, warning) = env
            .accounts_store_for(crate::harness::accounts::Cli::Codex)
            .unwrap()
            .load_logins(env.now)
            .unwrap();
        assert!(warning.is_none());
        let a = logins
            .enrollment
            .published_grant(&fixture.requests()[0].label, applied_a.generation.unwrap())
            .unwrap();
        let auth_directory = logins
            .enrollment
            .catalogue_staging(&a.request)
            .unwrap()
            .directory;
        super::super::super::catalogue::inventory_current_for_test(env)
            .unwrap_or_else(|why| panic!("{name}: the inventory before the answer: {why}"));
        // Retain original, intermediate-spent and current tokens before the
        // authenticated terminal refusal. Retirement must release none of them.
        fixture_promote_lineage_response(env, &a, &scope, "retirement-intermediate");
        fixture_promote_lineage_response(env, &a, &scope, "retirement-current");
        inject_lineage_answer(
            env,
            &a,
            &scope,
            Some(aterm_uds::rand::hex_token::<16>().unwrap()),
            (status, body),
        );
        let waits = super::super::super::catalogue::exchange_waits_for_test(env);
        if refused {
            assert_eq!(waits, Ok(true), "CONTROL {name}: {waits:?}");
        } else {
            assert!(waits.is_err(), "CONTROL {name}: {waits:?}");
        }
        let reconciled = reconcile_retained(env, &a, &scope);
        assert_eq!(reconciled.is_ok(), !refused, "{name}: {reconciled:?}");
        let claimed = Claimed::open(env, &a, false).unwrap().unwrap();
        let pending = claimed.journal.as_ref().unwrap().pending.as_ref().unwrap();
        let phase = aterm_json::to_string(&pending.phase).unwrap();
        assert_eq!(pending.phase == Phase::Rejected, refused, "{name}: {phase}");
        assert_eq!(
            pending.phase == Phase::Quarantined,
            !refused,
            "{name}: {phase}"
        );
        assert!(pending.quarantine_reason.is_none(), "{name}");
        assert!(pending.response.is_some(), "{name}");
        drop(claimed);
        let inventory = super::super::super::catalogue::inventory_current_for_test(env);
        assert!(inventory.is_ok(), "{name}: {inventory:?}");
        assert_eq!(
            super::super::super::catalogue::exchange_waits_for_test(env),
            Ok(false),
            "{name}: a terminal grant held another grant's code exchange"
        );
        assert_ne!(
            recorded_auth(env, &a.request.label),
            crate::harness::accounts::Auth::SignedOut,
            "CONTROL {name}: refusal settled before its publication window"
        );
        with_refresh_work_scoped(env, &scope, None, |work| {
            assert!(
                work.send_lineage(env, &a).is_err(),
                "{name}: no second send"
            );
            Ok(())
        })
        .unwrap();

        assert!(
            super::super::super::catalogue::refuses_same_principal_for_test(env, &a.request),
            "CONTROL {name}: a live or not-yet-settled member still fences its principal"
        );
        let mut later = env.clone();
        later.now = later
            .now
            .saturating_add(i64::try_from(codex_app_login::ROTATION_HANG.as_secs()).unwrap());
        if replace_auth {
            replace_canonical_refresh(&auth_directory, "replacement-refresh-token");
        }
        if refused && !replace_auth {
            later.now += 2;
            let newer_at = later.now - 1;
            record_contextual_auth(
                &later,
                &a.request.label,
                crate::harness::accounts::Auth::Ok,
                newer_at,
            );
            assert!(reconcile_retained(&later, &a, &scope).is_err());
            assert_eq!(
                recorded_auth(&later, &a.request.label),
                crate::harness::accounts::Auth::Ok,
            );
            let store = later
                .accounts_store_for(crate::harness::accounts::Cli::Codex)
                .unwrap();
            assert_eq!(
                store
                    .load(later.now)
                    .unwrap()
                    .0
                    .account(&a.request.label)
                    .auth_at,
                Some(newer_at)
            );
            assert_eq!(
                super::super::super::catalogue::phase_for_test(&later, &a.request),
                codex_enrollment::CataloguePhase::Received,
                "a newer signed-in observation cannot retire the member"
            );
            let newer_at = later.now;
            record_contextual_auth(
                &later,
                &a.request.label,
                crate::harness::accounts::Auth::SignedOut,
                newer_at,
            );
            super::super::super::catalogue::cut_retirement_once();
            assert!(reconcile_retained(&later, &a, &scope).is_err());
            assert_eq!(
                recorded_auth(&later, &a.request.label),
                crate::harness::accounts::Auth::SignedOut
            );
            assert!(
                super::super::super::catalogue::inventory_current_for_test(&later).is_err(),
                "entry-before-anchor interruption must not release an alias reservation"
            );
            assert!(
                super::super::super::catalogue::exchange_waits_for_test(&later).is_err(),
                "a new code exchange cannot pass the unreconciled retirement"
            );
            assert_eq!(
                store
                    .load(later.now)
                    .unwrap()
                    .0
                    .account(&a.request.label)
                    .auth_at,
                Some(newer_at),
                "settlement cannot roll back the newer contextual sign-out timestamp"
            );
            later.now += 1;
            record_contextual_auth(
                &later,
                &a.request.label,
                crate::harness::accounts::Auth::SignedOut,
                later.now,
            );
        }
        let settled = reconcile_retained(&later, &a, &scope);
        assert_eq!(settled.is_ok(), !replace_auth, "{name}: {settled:?}");
        if refused && !replace_auth {
            let store = later
                .accounts_store_for(crate::harness::accounts::Cli::Codex)
                .unwrap();
            assert_eq!(
                store
                    .load(later.now)
                    .unwrap()
                    .0
                    .account(&a.request.label)
                    .auth_at,
                Some(later.now),
                "reconciliation preserves a still newer sign-out across the entry/anchor cut"
            );
        }
        assert_eq!(
            recorded_auth(&later, &a.request.label) == crate::harness::accounts::Auth::SignedOut,
            refused && !replace_auth,
            "{name}: contextual sign-out result"
        );
        if !replace_auth {
            reconcile_retained(&later, &a, &scope)
                .unwrap_or_else(|why| panic!("{name}: idempotent settlement: {why:?}"));
            let next = super::super::super::row_fixture::prepare_row(
                &later,
                &"b".repeat(64),
                "alpha",
                'f',
                ("alpha", "alpha", "new-independent-refresh"),
            );
            assert_eq!(
                super::super::super::catalogue::phase_for_test(&later, &next),
                if refused {
                    codex_enrollment::CataloguePhase::Received
                } else {
                    codex_enrollment::CataloguePhase::Quarantined
                },
                "{name}: only settled rejected membership releases the principal"
            );
            assert!(
                super::super::super::catalogue::refuses_same_principal_for_test(
                    &later,
                    &fixture.requests()[1]
                ),
                "CONTROL {name}: the other live member still fences its principal"
            );
            if refused {
                let token = match name {
                    "invalid grant" => "test-refresh-alpha",
                    "expired refresh" => "retirement-intermediate",
                    _ => "retirement-current",
                };
                let roster = later
                    .accounts_roster_for(crate::harness::accounts::Cli::Codex)
                    .unwrap();
                let old = later.cwd.join("old-token-control");
                crate::harness::native_custody_platform::ensure_private_dir(&old).unwrap();
                let mut text = std::fs::read_to_string(&roster).unwrap();
                text.push_str(&format!("\n[[account]]\nlabel='token-control'\nemail='token-control@example.test'\nclass='dedicated'\ndir={}\n",
                    aterm_json::to_string(old.to_str().unwrap()).unwrap()));
                std::fs::write(&roster, text).unwrap();
                let alias = super::super::super::row_fixture::prepare_row(
                    &later,
                    &"b".repeat(64),
                    "token-control",
                    '1',
                    ("distinct-account", "distinct-user", token),
                );
                assert_eq!(
                    super::super::super::catalogue::phase_for_test(&later, &alias),
                    codex_enrollment::CataloguePhase::Quarantined,
                    "CONTROL {name}: retired original/current/spent token still denies a distinct principal"
                );
            }
        }
    }
}

fn operation() -> Operation {
    Operation {
        id: "a".repeat(32),
        caller: slot::Caller {
            sid: "s-fixture".into(),
            pid: 123,
            birth: 456,
        },
        slot: "b".repeat(64),
        expected_digest: "c".repeat(64),
        assignment_generation: 7,
        source_authority: SourceAuthority::Assignment {
            operation: "d".repeat(32),
        },
        credential_generation: 11,
    }
}

/// Synthetic received-boundary input; real owner/anchor/journal writes.
/// No network, Slot, Caller admission or provider-origin proof is minted.
#[cfg(unix)]
pub(super) fn inject_received_response(
    env: &crate::harness::cli::Env,
    binding: &super::super::GrantBinding,
    scope: &crate::harness::account_work::Scope,
) {
    // This generic promotion fixture models distinct issuer bundles. Alias
    // controls use inject_lineage_response with an explicit shared token.
    let refresh = format!("rotated-refresh-only-{}", binding.request.id);
    inject_lineage_response(env, binding, scope, None, &refresh)
}
#[cfg(unix)]
pub(super) fn inject_lineage_response(
    env: &crate::harness::cli::Env,
    binding: &super::super::GrantBinding,
    scope: &crate::harness::account_work::Scope,
    operation_id: Option<String>,
    refresh: &str,
) {
    let body = aterm_json::to_vec(
        &aterm_json::json!({"access_token":"fresh-access-only", "refresh_token":refresh}),
    )
    .unwrap();
    inject_lineage_answer(env, binding, scope, operation_id, (200, &body));
}

/// [`inject_lineage_response`] with the issuer's answer, `status` and `body`, as given.
#[cfg(unix)]
fn inject_lineage_answer(
    env: &crate::harness::cli::Env,
    binding: &super::super::GrantBinding,
    scope: &crate::harness::account_work::Scope,
    operation_id: Option<String>,
    (status, body): (u16, &[u8]),
) {
    super::super::super::with_maintenance_owner(env, binding, scope, |mut grant| {
        let mut witness = grant.anchor().read()?.ok_or("fixture witness missing")?;
        let mut claimed = Claimed::open(env, binding, false)?.ok_or("fixture journal missing")?;
        let current = claimed.view(&witness)?;
        let mut original = operation();
        if let Some(id) = operation_id {
            original.id = id;
        }
        original.expected_digest = current.revision.digest;
        original.credential_generation = 1;
        let token_digest =
            codex_app_login::owned_refresh_token_digest(current.document.as_slice())?;
        let journal = claimed.journal.as_mut().ok_or("fixture journal missing")?;
        journal.pending = Some(Pending {
            operation: original.into(),
            phase: Phase::Prepared,
            quarantine_reason: None,
            base_generation: journal.current.generation,
            base_digest: journal.current.digest.clone(),
            token_digest: Some(token_digest),
            response: None,
        });
        journal.transition = JournalTransition {
            expected: Some(witness),
            kind: TransitionKind::Attempt,
        };
        claimed = claimed
            .persist()
            .map_err(|_| "fixture reservation write failed")?;
        grant.anchor().compare_exchange(&claimed.transition()?)?;
        witness = grant
            .anchor()
            .read()?
            .ok_or("fixture reservation missing")?;
        claimed = claimed
            .arm_for_send(witness)
            .map_err(|_| "fixture attempted write failed")?;
        grant.anchor().compare_exchange(&claimed.transition()?)?;
        witness = grant.anchor().read()?.ok_or("fixture attempt missing")?;
        // Inject bytes at the received-response boundary: no network
        // function is called by this regression or by reconciliation.
        let journal = claimed.journal.as_mut().ok_or("fixture journal missing")?;
        let pending = journal.pending.as_mut().ok_or("fixture attempt missing")?;
        pending.phase = Phase::Responded;
        pending.response = Some(Response {
            status,
            received_at: Some(env.now as u64),
            body: aterm_codec::base64::encode(body).map_err(|_| "fixture encoding failed")?,
        });
        journal.transition = JournalTransition {
            expected: Some(witness),
            kind: TransitionKind::Response,
        };
        claimed = claimed
            .persist()
            .map_err(|_| "fixture response write failed")?;
        grant.anchor().compare_exchange(&claimed.transition()?)?;
        Ok(())
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn cancelled_sender_durable_response_promotes_shared_owner_once_without_delivery() {
    // Synthetic provider completion; all account, issuer-seal, encrypted
    // journal and canonical generation/witness paths are real. This does
    // not qualify OAuth or the prospective source-publication barrier.
    let fixture = super::super::super::maintenance_tests::fixture();
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    inject_received_response(&fixture.env, &fixture.binding, &scope);
    scope.cancellation().cancel();
    assert!(scope.cancelled());
    for _ in 0..2 {
        assert!(reconcile_retained(&fixture.env, &fixture.binding, &scope).is_ok());
    }
    super::super::super::with_maintenance_owner(
        &fixture.env,
        &fixture.binding,
        &scope,
        |mut grant| {
            let witness = grant.anchor().read()?.ok_or("settled witness missing")?;
            assert_eq!(witness.revision.generation, 2);
            assert!(witness.pending.is_none());
            let claimed = Claimed::open(&fixture.env, &fixture.binding, false)?
                .ok_or("settled journal missing")?;
            let current = claimed.view(&witness)?;
            assert_eq!(current.revision.generation, 2);
            assert!(std::fs::read(&fixture.auth).unwrap() == current.document.as_slice());
            let journal = claimed.journal.as_ref().unwrap();
            assert!(journal.pending.is_none());
            assert!(journal.previous_current.is_none());
            assert_eq!(journal.completed.len(), 1);
            assert_eq!(journal.completed[0].operation.id(), "a".repeat(32));
            assert!(journal.completed[0].spent_token_digest.is_some());
            Ok(())
        },
    )
    .unwrap();
    let store = fixture
        .env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    assert!(!store.dir.join("session-slots").exists());
    assert!(!scope.cancellation().retired());
}

#[test]
fn immutable_delivery_digest_binds_every_caller_assignment_and_request_field() {
    let original = operation();
    let expected = operation_digest(&original).unwrap();
    for field in 0..8 {
        let mut changed = original.clone();
        match field {
            0 => changed.id = "e".repeat(32),
            1 => changed.caller.sid = "s-other".into(),
            2 => changed.caller.pid += 1,
            3 => changed.caller.birth += 1,
            4 => changed.slot = "e".repeat(64),
            5 => changed.expected_digest = "e".repeat(64),
            6 => changed.assignment_generation += 1,
            _ => {
                changed.source_authority = SourceAuthority::Assignment {
                    operation: "e".repeat(32),
                }
            }
        }
        assert_ne!(operation_digest(&changed).unwrap(), expected);
        assert!(!same_wire_operation(&original, &changed));
    }
    let mut observed = original.clone();
    observed.credential_generation += 1;
    assert!(
        same_wire_operation(&original, &observed),
        "lost ACK can advance the authenticated slot fact"
    );
    assert_ne!(
        operation_digest(&observed).unwrap(),
        expected,
        "the recorded attempt still binds its original fact"
    );
}

#[test]
fn startup_refresh_correlation_is_distinct_from_an_assignment_intent() {
    let mut startup = operation();
    startup.assignment_generation = 0;
    assert!(!startup.valid());
    startup.source_authority = SourceAuthority::Startup {
        nonce: "d".repeat(32),
    };
    assert!(startup.valid());
    assert!(!same_wire_operation(&operation(), &startup));
    assert_ne!(
        operation_digest(&operation()).unwrap(),
        operation_digest(&startup).unwrap()
    );
    let mut changed = startup.clone();
    changed.source_authority = SourceAuthority::Startup {
        nonce: "e".repeat(32),
    };
    assert!(!same_wire_operation(&startup, &changed));
    changed.credential_generation = 0;
    assert!(!changed.valid());
}

#[test]
#[cfg(unix)]
fn missing_assignment_book_refuses_the_actual_refresh_send_for_both_source_modes() {
    let fixture = super::super::super::maintenance_tests::fixture();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let document = aterm_json::to_vec(&aterm_json::json!({
        "auth_mode":"chatgpt","tokens":{
            "access_token":"synthetic-source-guard-access",
            "refresh_token":"synthetic-source-guard-refresh",
            "account_id":"synthetic-source-guard-account",
            "oauth_provenance":{
                "issuer":format!("http://{}",listener.local_addr().unwrap()),
                "client_id":codex_app_login::CLIENT_ID
            }
        }
    }))
    .unwrap();
    for startup in [false, true] {
        let mut operation = operation();
        if startup {
            operation.assignment_generation = 0;
            operation.source_authority = SourceAuthority::Startup {
                nonce: "d".repeat(32),
            };
        }
        assert!(operation.valid());
        let mut checked = false;
        let result = codex_app_login::exchange_owned_refresh(
            &document,
            crate::harness::auth_deadline::Deadline::after(aterm_spec::wait::MUST_HAPPEN),
            || {
                checked = true;
                current_source(&fixture.env, &fixture.binding, &operation, None)
            },
        );
        assert!(checked, "the real pre-send admission was reached");
        assert!(result.is_err());
        assert_eq!(
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap_err()
            .kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
