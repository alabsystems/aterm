// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn real_global_custody_quarantines_principal_and_retained_token_aliases() {
    for bundles in [
        [
            ("account-a", "user-a", "rt-shared"),
            ("account-b", "user-b", "rt-shared"),
        ],
        [
            ("account-a", "user-shared", "rt-a"),
            ("account-b", "user-shared", "rt-b"),
        ],
        [
            ("account-shared", "user-a", "rt-a"),
            ("account-shared", "user-b", "rt-b"),
        ],
    ] {
        let (_world, env, [a, b]) =
            super::super::row_fixture::create_with_lineage(&"b".repeat(64), bundles);
        let a_scope = existing_scope(&env, &a.id).unwrap().unwrap();
        let b_scope = existing_scope(&env, &b.id).unwrap().unwrap();
        assert_eq!(
            ClaimedEntry::open(&env, &a_scope, false)
                .unwrap()
                .entry
                .unwrap()
                .phase,
            CataloguePhase::Received
        );
        assert_eq!(
            ClaimedEntry::open(&env, &b_scope, false)
                .unwrap()
                .entry
                .unwrap()
                .phase,
            CataloguePhase::Quarantined
        );
        let known = Inventory::read(&env, Some(&a_scope)).unwrap();
        let first = ClaimedEntry::open(&env, &a_scope, false).unwrap();
        let original_facts = first.entry.as_ref().unwrap().facts.as_ref().unwrap();
        assert!(
            known.refuses_alias(original_facts),
            "the existing aliased owner must be fenced too"
        );
        assert!(
            known.members.iter().any(|member| member.pending),
            "known conflict must still fence unknown code exchanges"
        );
        let mut unrelated = original_facts.clone();
        unrelated.principal = Identity::from_native("separate-account", "separate-user");
        unrelated.token = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
            b"synthetic-distinct-token-fact",
        ));
        assert!(
            !known.refuses_alias(&unrelated),
            "complete known alias facts must not turn every distinct bundle into unknown"
        );
        assert!(recover_pending(&env, &a).is_ok());
        assert!(
            recover_pending(&env, &b).is_ok(),
            "quarantine must preserve the exact completed encrypted response"
        );
    }
}

#[test]
fn actual_global_catalogue_observes_distinct_bundles_and_detects_entry_loss() {
    let (_world, env, requests) = super::super::row_fixture::create(&"b".repeat(64));
    let inventory = Inventory::read(&env, None).unwrap();
    assert_eq!(inventory.members.len(), 2);
    assert!(
        !inventory.members[0]
            .facts
            .overlaps_principal(&inventory.members[1].facts)
    );
    let scope = existing_scope(&env, &requests[0].id).unwrap().unwrap();
    let authority = Authority::open(&env, false).unwrap();
    let directory = authority.directory.join(&requests[0].id).join("catalogue");
    drop(authority);
    let displaced = directory.with_extension("held");
    std::fs::rename(&directory, &displaced).unwrap();
    assert!(
        Inventory::read(&env, None).is_err(),
        "manifest membership must prevent missing-entry absence from meaning no alias"
    );
    std::fs::rename(displaced, directory).unwrap();
    assert!(Inventory::read(&env, None).is_ok());
    assert_eq!(scope.request(), &requests[0]);
}

#[test]
fn untouched_membership_anchor_refuses_actual_old_manifest_ciphertext() {
    let old = std::cell::RefCell::new(None);
    let (_world, env, _) = super::super::row_fixture::create_with_lineage_observed(
        &"b".repeat(64),
        [("alpha", "alpha", "rt-a"), ("beta", "beta", "rt-b")],
        |env| {
            let authority = Authority::open(env, false).unwrap();
            let path = authority
                .source
                .directory()
                .unwrap()
                .join(AUTHORITY)
                .join("candidate.age");
            *old.borrow_mut() = Some(std::fs::read(path).unwrap());
        },
    );
    let authority = Authority::open(&env, false).unwrap();
    let path = authority
        .source
        .directory()
        .unwrap()
        .join(AUTHORITY)
        .join("candidate.age");
    let current = std::fs::read(&path).unwrap();
    drop(authority);
    std::fs::write(&path, old.into_inner().unwrap()).unwrap();
    assert!(
        Inventory::read(&env, None).is_err(),
        "valid old manifest must not omit the untouched admitted second issuer"
    );
    std::fs::write(path, current).unwrap();
    assert_eq!(Inventory::read(&env, None).unwrap().members.len(), 2);
}

#[test]
fn real_issuer_resources_exclude_shared_user_or_token_but_allow_distinct_bundles() {
    for (bundles, conflicts) in [
        (
            [
                ("account-a", "shared-user", "rt-a"),
                ("account-b", "shared-user", "rt-b"),
            ],
            true,
        ),
        (
            [
                ("account-a", "user-a", "shared-rt"),
                ("account-b", "user-b", "shared-rt"),
            ],
            true,
        ),
        (
            [
                ("account-a", "user-a", "rt-a"),
                ("account-b", "user-b", "rt-b"),
            ],
            false,
        ),
    ] {
        let (_world, env, [a, b]) =
            super::super::row_fixture::create_with_lineage(&"b".repeat(64), bundles);
        let ga = recover_pending(&env, &a).unwrap();
        let gb = recover_pending(&env, &b).unwrap();
        let proof_a = publication_identity(&env, &a, &ga).unwrap();
        let proof_b = publication_identity(&env, &b, &gb).unwrap();
        let source = crate::harness::codex_source_gate::SourceWorkLease::for_home(
            env.home.as_deref().unwrap(),
        )
        .unwrap();
        let _exchange =
            crate::harness::native_enrollment_admission::RefreshExchangeLease::for_env(&env)
                .unwrap();
        let first = crate::harness::native_enrollment_admission::RefreshResourceTurn::try_acquire(
            source.namespace().unwrap(),
            &proof_a,
        )
        .unwrap()
        .unwrap();
        let second = crate::harness::native_enrollment_admission::RefreshResourceTurn::try_acquire(
            source.namespace().unwrap(),
            &proof_b,
        )
        .unwrap();
        assert_eq!(
            second.is_none(),
            conflicts,
            "kernel keys come from authenticated synthetic issuer bundles, not rows/UUIDs"
        );
        first.current().unwrap();
        drop(second);
        drop(first);
        assert!(
            crate::harness::native_enrollment_admission::RefreshResourceTurn::try_acquire(
                source.namespace().unwrap(),
                &proof_b
            )
            .unwrap()
            .is_some()
        );
    }
}

#[test]
fn global_history_keeps_intermediate_spent_tokens_without_taking_live_profile_claims() {
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
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    refresh::fixture_promote_lineage_response(env, &binding, &scope, "test-intermediate-refresh");
    let (issuer, original) = open_authenticated(env, &binding.request, &binding.reference).unwrap();
    let digest =
        codex_app_login::owned_refresh_token_digest(&std::fs::read(&original.stage).unwrap())
            .unwrap();
    drop(issuer);
    refresh::fixture_promote_lineage_response(env, &binding, &scope, "test-current-refresh");
    let (directory, _) = Store::location(env, &request.id).unwrap();
    // Real HTTP executors hold these actual Profile claims. The inventory is
    // read-only and must authenticate without a cross-owner lock dependency.
    let _issuer = crate::harness::browser_window::ProfileLock::claim(&directory).unwrap();
    let _refresh =
        crate::harness::browser_window::ProfileLock::claim(&directory.join("refresh")).unwrap();
    let inventory = Inventory::read(env, None).unwrap();
    let member = inventory
        .members
        .iter()
        .find(|member| member.facts.principal == Identity::from_native("alpha", "alpha"))
        .unwrap();
    assert!(
        member.tokens.contains(&digest),
        "C1 and C3 alone must not forget a spent C2 token"
    );
    let mut different_principal = inventory
        .members
        .iter()
        .find(|member| member.facts.principal == Identity::from_native("beta", "beta"))
        .unwrap()
        .facts
        .clone();
    different_principal.principal = Identity::from_native("third-account", "third-user");
    different_principal.token = digest;
    assert!(
        inventory.refuses_alias(&different_principal),
        "algorithm refuses a spent-token alias even with distinct user/account fields"
    );
}

#[test]
fn anchored_initial_metadata_cut_rolls_forward_exact_empty_namespace() {
    let (_world, env, _, _) = super::super::tests::fixture(true);
    MANIFEST_CUT.with(|cut| cut.set(2));
    assert!(Authority::open(&env, true).is_err());
    let namespace = crate::harness::codex_source_gate::existing_namespace_for_home(
        env.home.as_deref().unwrap(),
    )
    .unwrap();
    let parent = namespace.directory().unwrap();
    let identity = platform::machine_identity(env.home.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let anchor =
        MembershipAnchor::open(parent, env.home.as_deref().unwrap(), &identity, false).unwrap();
    let expected = anchor
        .bootstrap_manifest(&parent.join(OWNERS))
        .unwrap()
        .unwrap();
    drop(anchor);
    let recovered = Authority::open(&env, false).unwrap();
    assert_eq!(recovered.manifest.namespace, expected.namespace);
    assert_eq!(recovered.manifest.sequence, 1);
    assert!(recovered.manifest.members.is_empty());
    recovered.current().unwrap();
}

#[test]
fn pending_manifest_metadata_rolls_forward_without_minting_a_native_admission() {
    let (_world, env, requests) = super::super::row_fixture::create(&"b".repeat(64));
    let mut authority = Authority::open(&env, false).unwrap();
    let mut intended = authority
        .manifest
        .members
        .get(&requests[0].id)
        .unwrap()
        .clone();
    let mut nonce = [0u8; 16];
    aterm_uds::rand::fill(&mut nonce).unwrap();
    intended.source.request.id = aterm_codec::hex::encode_lower(&nonce);
    nonce.zeroize();
    // Metadata-only proposed membership is not canonical request/grant proof.
    // The private save path exercises actual MACed payload/CAS crash recovery.
    authority.manifest.sequence += 1;
    authority
        .manifest
        .members
        .insert(intended.request().id.clone(), intended.clone());
    MANIFEST_CUT.with(|cut| cut.set(1));
    assert!(authority.save().is_err());
    drop(authority);
    let recovered = Authority::open(&env, false).unwrap();
    assert!(recovered.manifest.members.get(&intended.request().id) == Some(&intended));
    recovered.current().unwrap();
    drop(recovered);
    assert!(
        attempted(&env, &intended).is_err(),
        "a recovered metadata proposal cannot mint the missing canonical native admission"
    );
    assert!(
        Inventory::read(&env, None).is_err(),
        "unknown membership must not be evicted or treated as an independent grant"
    );
}

#[test]
fn machine_home_catalogue_joins_real_distinct_canonical_state_roots() {
    for (bundle, phase) in [
        (
            ("gamma", "gamma", "test-refresh-gamma"),
            CataloguePhase::Received,
        ),
        (
            ("gamma", "gamma", "test-refresh-alpha"),
            CataloguePhase::Quarantined,
        ),
    ] {
        let (_world, first, second, _, request) =
            super::super::row_fixture::create_second_state(&"b".repeat(64), bundle);
        assert_ne!(first.state, second.state);
        assert_ne!(first.config, second.config);
        assert_eq!(first.home, second.home);
        let scope = existing_scope(&second, &request.id).unwrap().unwrap();
        let observed = ClaimedEntry::open(&second, &scope, false).unwrap();
        assert_eq!(observed.entry.as_ref().unwrap().phase, phase);
        let facts = observed.entry.as_ref().unwrap().facts.clone().unwrap();
        drop(observed);
        let inventory = Inventory::read(&first, None).unwrap();
        assert_eq!(inventory.members.len(), 3);
        if phase == CataloguePhase::Quarantined {
            assert!(inventory.members.iter().any(|member| member.pending));
            assert!(
                inventory.refuses_alias(&facts),
                "a foreign-state retained alias cannot disappear from the first state's inventory"
            );
        }
    }
}

#[test]
fn authenticated_unknown_code_attempt_is_not_an_owned_refresh_projection() {
    super::super::row_fixture::with_global_code_fixture(
        |env, request, _, turn, mut prepared, _artifact| {
            crate::harness::codex_app_login::CodeGrantCapture::begin_exchange(&mut prepared)
                .unwrap();
            let scope = existing_scope(env, &request.id).unwrap().unwrap();
            let claimed = ClaimedEntry::open(env, &scope, false).unwrap();
            let entry = claimed.entry.as_ref().unwrap();
            assert_eq!(entry.phase, CataloguePhase::Attempted);
            assert!(entry.facts.is_none());
            drop(claimed);
            drop(prepared);
            drop(turn);
            assert!(
                Inventory::read(env, None).is_err(),
                "unbound Code attempt must not become scoped refresh or absence evidence"
            );
        },
    );
}

#[test]
fn authenticated_catalogue_observation_does_not_claim_writer_profiles_or_registry() {
    let (_world, env, requests) = super::super::row_fixture::create(&"e".repeat(64));
    let scope = existing_scope(&env, &requests[0].id).unwrap().unwrap();
    let claimed = ClaimedEntry::open(&env, &scope, false).unwrap();
    let authority = Authority::open(&env, false).unwrap();
    let _registry = platform::registry_lock(authority.source.directory().unwrap()).unwrap();
    let observed = AuthorityObservation::read(&env).unwrap();
    assert!(
        aterm_json::to_vec(&observed.manifest).unwrap()
            == aterm_json::to_vec(&authority.manifest).unwrap()
    );
    let entry = EntryObservation::read(&env, &scope).unwrap();
    assert!(entry.witness().unwrap() == claimed.witness().unwrap());
    observed.current().unwrap();
    entry.store.unchanged().unwrap();
}

#[cfg(unix)]
#[test]
fn readonly_catalogue_fd_cannot_be_relabelled_by_byte_identical_directory_copy() {
    let (_world, env, _) = super::super::row_fixture::create(&"f".repeat(64));
    let observed = AuthorityObservation::read(&env).unwrap();
    let path = observed.directory.parent().unwrap().join(AUTHORITY);
    let displaced = path.with_extension("readonly-displaced");
    let original = std::fs::read(path.join("candidate.age")).unwrap();
    std::fs::rename(&path, &displaced).unwrap();
    platform::ensure_private_dir(&path).unwrap();
    std::fs::copy(displaced.join("candidate.age"), path.join("candidate.age")).unwrap();
    assert_eq!(std::fs::read(path.join("candidate.age")).unwrap(), original);
    assert!(
        observed.current().is_err(),
        "copied bytes do not relabel the opened observation"
    );
    std::fs::remove_file(path.join("candidate.age")).unwrap();
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&displaced, &path).unwrap();
    observed.current().unwrap();
}

#[test]
fn native_flag_without_actual_staging_cannot_create_catalogue_membership() {
    let (_world, env, auth, request) = super::super::tests::fixture(true);
    let store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    store
        .transact_logins(env.now, &mut None, |_, logins| {
            logins.enrollment.admit_native(&request)?;
            Ok(Vec::new())
        })
        .unwrap();
    let before = store.load_logins(env.now).unwrap().0.to_json();
    assert!(prepare(&env, &request, &auth).is_err());
    assert_eq!(store.load_logins(env.now).unwrap().0.to_json(), before);
    assert!(!auth.exists());
    assert!(AuthorityObservation::read(&env).is_err());
}

#[test]
fn historical_native_admission_cannot_start_a_new_exchange_without_migration() {
    let (_world, env, auth, request) = super::super::tests::fixture(true);
    let store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .unwrap();
    store
        .transact_logins(env.now, &mut None, |_, logins| {
            logins
                .enrollment
                .admit_historical_native_fixture(&request)?;
            Ok(Vec::new())
        })
        .unwrap();
    let before = store.load_logins(env.now).unwrap().0.to_json();
    assert_eq!(
        prepare(&env, &request, &auth).unwrap_err(),
        "historical native issuance requires authenticated migration"
    );
    assert_eq!(store.load_logins(env.now).unwrap().0.to_json(), before);
    assert!(!auth.exists());
    assert!(AuthorityObservation::read(&env).is_err());
}
