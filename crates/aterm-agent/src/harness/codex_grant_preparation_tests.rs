// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn refuse(env: &Env, request: &codex_enrollment::Request) {
    env.accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap()
        .transact_logins(env.now, &mut None, |_, logins| {
            logins.enrollment.refuse_preparation(
                request,
                crate::harness::login_drive::NativePreparationStage::GrantCustody,
            )?;
            Ok(Vec::new())
        })
        .unwrap();
}

#[test]
fn busy_writer_refuses_before_membership_and_borrowed_writer_prepares() {
    super::super::row_fixture::with_preparation_fixture(
        |env, request, auth, _turn, _admission, _artifact| {
            let store = env
                .accounts_store_for(crate::harness::accounts::Cli::Codex)
                .unwrap();
            let before = AuthorityObservation::read(env).unwrap().manifest;
            let held = store.acquire().unwrap();
            assert!(prepare(env, request, auth).is_err());
            let after = AuthorityObservation::read(env).unwrap().manifest;
            assert_eq!(
                aterm_json::to_vec(&before).unwrap(),
                aterm_json::to_vec(&after).unwrap()
            );
            assert!(!after.members.contains_key(&request.id));
            let scope = prepare_held(env, request, auth, &store, &held)
                .unwrap()
                .unwrap();
            assert!(scope.guarded_preparation);
            assert!(
                store
                    .load_logins(env.now)
                    .unwrap()
                    .0
                    .enrollment
                    .catalogue_witness(request)
                    .unwrap()
                    .is_some()
            );
        },
    );
}

#[test]
fn interrupted_refused_preparation_repairs_only_inert_metadata_even_after_stage_removal() {
    for cut in [1, 2] {
        super::super::row_fixture::with_preparation_fixture(
            |env, request, auth, _turn, _admission, _artifact| {
                let store = env
                    .accounts_store_for(crate::harness::accounts::Cli::Codex)
                    .unwrap();
                let baseline = Inventory::read(env, None).unwrap().members.len();
                PREPARATION_CUT.with(|value| value.set(cut));
                assert!(prepare(env, request, auth).is_err());
                let scope =
                    AuthorityObservation::read(env).unwrap().manifest.members[&request.id].clone();
                assert!(Inventory::read(env, None).is_err());
                refuse(env, request);
                std::fs::remove_dir_all(auth.parent().unwrap()).unwrap();
                let held = store.acquire().unwrap();
                let mut foreign = env.clone();
                foreign.state = env.state.join("different-source");
                let other = foreign
                    .accounts_store_for(crate::harness::accounts::Cli::Codex)
                    .unwrap();
                assert!(reconcile_preparation_held(env, &scope, &other, &held).is_err());
                reconcile_preparation_held(env, &scope, &store, &held).unwrap();
                let logins = store.load_logins(env.now).unwrap().0;
                assert_eq!(
                    logins.enrollment.lookup(request).unwrap().unwrap().phase,
                    codex_enrollment::Phase::Refused
                );
                assert_eq!(
                    logins
                        .enrollment
                        .catalogue_witness(request)
                        .unwrap()
                        .unwrap()
                        .phase,
                    CataloguePhase::Prepared
                );
                assert!(!auth.parent().unwrap().exists());
                assert_eq!(Inventory::read(env, None).unwrap().members.len(), baseline);
                assert!(
                    super::super::recover_not_dispatched(env, request)
                        .unwrap()
                        .is_none()
                );
                let original = std::fs::read(store.logins_file()).unwrap();
                reconcile_preparation_held(env, &scope, &store, &held).unwrap();
                assert_eq!(std::fs::read(store.logins_file()).unwrap(), original);
            },
        );
    }
}

#[test]
fn live_issuer_claim_and_retained_body_block_preparation_repair() {
    super::super::row_fixture::with_preparation_fixture(
        |env, request, auth, _turn, _admission, _artifact| {
            let store = env
                .accounts_store_for(crate::harness::accounts::Cli::Codex)
                .unwrap();
            PREPARATION_CUT.with(|value| value.set(1));
            assert!(prepare(env, request, auth).is_err());
            let observation = AuthorityObservation::read(env).unwrap();
            let scope = observation.manifest.members[&request.id].clone();
            let directory = observation.directory.join(&request.id);
            refuse(env, request);
            let (owner, _) =
                platform::Store::open_owned(&directory, env.home.as_deref().unwrap(), MAX_ENVELOPE)
                    .unwrap();
            let held = store.acquire().unwrap();
            let before = std::fs::read(store.logins_file()).unwrap();
            assert!(reconcile_preparation_held(env, &scope, &store, &held).is_err());
            assert_eq!(std::fs::read(store.logins_file()).unwrap(), before);
            drop(owner);
            reconcile_preparation_held(env, &scope, &store, &held).unwrap();
        },
    );
    super::super::row_fixture::with_global_code_fixture(
        |env, request, _auth, _turn, pending, _artifact| {
            let store = env
                .accounts_store_for(crate::harness::accounts::Cli::Codex)
                .unwrap();
            let scope = pending.record.catalogue_scope.clone().unwrap();
            drop(pending);
            // Simulate an old mutable Book rollback without erasing actual issuer custody.
            let mut book = store.load_logins(env.now).unwrap().0;
            book.enrollment
                .finish(request, codex_enrollment::Phase::Refused)
                .unwrap();
            let mut encoded = book.enrollment.encode();
            encoded
                .as_object_mut()
                .unwrap()
                .get_mut(&request.id)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("catalogue_witness");
            book.enrollment = codex_enrollment::Operations::decode(&encoded).unwrap();
            std::fs::write(store.logins_file(), book.to_json()).unwrap();
            let held = store.acquire().unwrap();
            assert!(reconcile_preparation_held(env, &scope, &store, &held).is_err());
        },
    );
}

#[test]
fn independent_no_attempt_history_refuses_joint_book_entry_and_manifest_rollbacks() {
    for erase_entry in [false, true] {
        super::super::row_fixture::with_preparation_fixture(
            |env, request, auth, _turn, _admission, _artifact| {
                let store = env
                    .accounts_store_for(crate::harness::accounts::Cli::Codex)
                    .unwrap();
                let mut rollback = store.load_logins(env.now).unwrap().0;
                rollback
                    .enrollment
                    .finish(request, codex_enrollment::Phase::Refused)
                    .unwrap();
                let mut pending =
                    super::super::prepare(env, request, auth, &"d".repeat(64)).unwrap();
                let scope = pending.record.catalogue_scope.clone().unwrap();
                let authority = AuthorityObservation::read(env).unwrap();
                let entry = authority
                    .directory
                    .join(&request.id)
                    .join("catalogue/candidate.age");
                let old_entry = std::fs::read(&entry).unwrap();
                let manifest_path = crate::harness::codex_source_gate::existing_namespace_for_home(
                    env.home.as_deref().unwrap(),
                )
                .unwrap()
                .directory()
                .unwrap()
                .join(AUTHORITY)
                .join("candidate.age");
                let old_manifest = std::fs::read(&manifest_path).unwrap();
                pending.begin_exchange().unwrap();
                drop(pending);
                std::fs::write(store.logins_file(), rollback.to_json()).unwrap();
                if erase_entry {
                    std::fs::remove_file(&entry).unwrap();
                } else {
                    std::fs::write(&entry, &old_entry).unwrap();
                }
                let held = store.acquire().unwrap();
                assert!(reconcile_preparation_held(env, &scope, &store, &held).is_err());
                let current_manifest = std::fs::read(&manifest_path).unwrap();
                std::fs::write(&manifest_path, old_manifest).unwrap();
                assert!(
                    AuthorityObservation::read(env).is_err(),
                    "independent membership high-water refuses old positive fences"
                );
                assert!(reconcile_preparation_held(env, &scope, &store, &held).is_err());
                std::fs::write(manifest_path, current_manifest).unwrap();
            },
        );
    }
}

#[test]
fn legacy_unfenced_partial_and_foreign_source_never_gain_repair_authority() {
    super::super::row_fixture::with_preparation_fixture(
        |env, request, auth, _turn, _admission, _artifact| {
            let store = env
                .accounts_store_for(crate::harness::accounts::Cli::Codex)
                .unwrap();
            let staging = store
                .load_logins(env.now)
                .unwrap()
                .0
                .enrollment
                .catalogue_staging(request)
                .unwrap();
            let mut authority = Authority::open(env, false).unwrap();
            let scope = CatalogueScope {
                namespace: authority.manifest.namespace.clone(),
                admission: "e".repeat(64),
                source: staging,
                guarded_preparation: false,
            };
            authority.manifest.sequence += 1;
            authority
                .manifest
                .members
                .insert(request.id.clone(), scope.clone());
            authority.save().unwrap();
            drop(authority);
            assert!(
                prepare(env, request, auth).is_err(),
                "an unfenced existing scope cannot be re-prepared"
            );
            let directory = location(env, &scope).unwrap();
            let (issuer, _) =
                platform::Store::open_owned(&directory, env.home.as_deref().unwrap(), MAX_ENVELOPE)
                    .unwrap();
            {
                let held = store.acquire().unwrap();
                let mut entry = ClaimedEntry::open(env, &scope, true).unwrap();
                entry
                    .save(Entry {
                        schema: 1,
                        scope: scope.clone(),
                        sequence: 1,
                        predecessor: None,
                        phase: CataloguePhase::Prepared,
                        reference: None,
                        facts: None,
                        response_digest: None,
                        retirement: None,
                    })
                    .unwrap();
                let mut anchor = CatalogueAnchorWriter::new(&store, &held, request, env.now);
                anchor
                    .compare_exchange(&CatalogueTransition {
                        scope: scope.clone(),
                        expected: None,
                        next: entry.witness().unwrap(),
                    })
                    .unwrap();
            }
            drop(issuer);
            assert!(
                Inventory::read(env, None).is_err(),
                "matching legacy Prepared metadata is not an independent no-attempt proof"
            );
            refuse(env, request);
            let held = store.acquire().unwrap();
            let before = std::fs::read(store.logins_file()).unwrap();
            assert!(reconcile_preparation_held(env, &scope, &store, &held).is_err());
            assert_eq!(std::fs::read(store.logins_file()).unwrap(), before);
            let mut foreign = env.clone();
            foreign.state = env.state.join("foreign-source");
            let other = foreign
                .accounts_store_for(crate::harness::accounts::Cli::Codex)
                .unwrap();
            assert!(reconcile_preparation_held(env, &scope, &other, &held).is_err());
        },
    );
}
