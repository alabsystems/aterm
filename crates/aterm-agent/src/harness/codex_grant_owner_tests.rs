// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CANARY: &[u8] = br#"{"tokens":{"access_token":"only-a-test-canary","refresh_token":"only-a-test-refresh"},"unknown_native_field":true}"#;

pub(super) fn fixture(
    key: bool,
) -> (
    aterm_tempfile::TempDir,
    Env,
    PathBuf,
    codex_enrollment::Request,
) {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let stage = root.path().join("stage");
    aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&stage).unwrap();
    if key {
        client::machine_identity_as(
            &client::Paths::locate_for_home(&home),
            aterm_secrets::Making {
                test_binary: true,
                ..aterm_secrets::Making::default()
            },
        )
        .unwrap();
    }
    let env = Env {
        state: root.path().join("state"),
        aterm_state: Some(root.path().join("aterm")),
        cwd: home.clone(),
        home: Some(home),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        codex_login: None,
        agy_conversation: None,
        now: 1_790_000_000,
        sid: "s-test-owner".into(),
        sock: None,
        config: None,
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    };
    let request = codex_enrollment::Request {
        id: "a".repeat(32),
        caller: "b".repeat(64),
        label: "fixture".into(),
        revision: "c".repeat(64),
    };
    (root, env, stage.join("auth.json"), request)
}

fn prepared(env: &Env, stage: &Path, request: &codex_enrollment::Request) -> PreparedGrant {
    prepare(env, request, stage, &"d".repeat(64)).unwrap()
}

// This test-only injection exercises encrypted storage/recovery. Production
// verified capture is available only through the actual FreshCodeGrant producer.
fn fixture_complete(mut pending: PreparedGrant) -> GrantReference {
    pending.begin_exchange().unwrap();
    pending.capture_response(
        CANARY,
        Some(Identity::from_native("account", "user")),
        Some("fixture-code-owner".into()),
    );
    match pending.settle_response() {
        CaptureOutcome::Verified(grant) => grant.reference(),
        other => panic!("fixture failed: {other:?}"),
    }
}

#[test]
fn preparation_never_mints_a_machine_key_and_uncertain_exchange_cannot_restart() {
    let (_root, env, stage, request) = fixture(false);
    assert!(prepare(&env, &request, &stage, &"d".repeat(64)).is_err());
    assert!(
        client::read_machine_identity(&client::Paths::locate_for_home(
            env.home.as_deref().unwrap()
        ))
        .unwrap()
        .is_none()
    );
    let (_root, env, stage, request) = fixture(true);
    let mut pending = prepared(&env, &stage, &request);
    pending.begin_exchange().unwrap();
    drop(pending);
    assert!(prepare(&env, &request, &stage, &"d".repeat(64)).is_err());
    assert!(!stage.exists(), "no stage credential was published");
}

#[test]
fn independent_enrollments_do_not_share_a_lock_or_steal_an_operation() {
    let (_root, env, stage, request) = fixture(true);
    let first = prepared(&env, &stage, &request);
    assert!(prepare(&env, &request, &stage, &"d".repeat(64)).is_err());
    let mut another = request.clone();
    another.id = "e".repeat(32);
    let second = prepared(&env, &stage, &another);
    drop((first, second));
    let mut changed = request.clone();
    changed.caller = "f".repeat(64);
    assert!(prepare(&env, &changed, &stage, &"d".repeat(64)).is_err());
}

#[test]
fn private_reference_authenticates_exact_issuance_and_survives_missing_plain_stage() {
    let (_root, env, stage, request) = fixture(true);
    let reference = fixture_complete(prepared(&env, &stage, &request));
    let (directory, _) = Store::location(&env, &request.id).unwrap();
    let ciphertext = std::fs::read(directory.join("candidate.age")).unwrap();
    assert!(
        !ciphertext
            .windows(b"only-a-test-canary".len())
            .any(|part| part == b"only-a-test-canary")
    );
    assert!(!format!("{reference:?}").contains(&reference.capability));
    assert!(recover(&env, &request, &reference).is_ok());
    assert_eq!(
        recover_pending(&env, &request).unwrap().reference(),
        reference
    );
    std::fs::remove_dir(stage.parent().unwrap()).unwrap();
    assert!(
        recover(&env, &request, &reference).is_ok(),
        "encrypted issuance does not depend on a copied plain stage"
    );
    let mut changed = request.clone();
    changed.caller = "f".repeat(64);
    assert!(recover(&env, &changed, &reference).is_err());
}

#[test]
fn public_recipient_reencryption_cannot_forge_a_fresh_code_response() {
    let (_root, env, stage, request) = fixture(true);
    let reference = fixture_complete(prepared(&env, &stage, &request));
    let (store, record) = Store::open(&env, &request.id, false).unwrap().unwrap();
    let mut record = record.unwrap();
    let original = std::fs::read(store.directory.join("candidate.age")).unwrap();
    record.response.as_mut().unwrap().identity =
        Some(Identity::from_native("forged-account", "forged-user"));
    record.response.as_mut().unwrap().document =
        aterm_codec::base64::encode(br#"{"forged":"document"}"#).unwrap();
    // The attacker uses ONLY the public recipient, preserving operation,
    // request and the copied recovery capability in a valid new age envelope.
    let bytes = SecretBytes::new(aterm_json::to_vec(&record).unwrap());
    let replacement = age::encrypt(&[store.identity.recipient()], bytes.as_slice()).unwrap();
    let path = store.directory.join("candidate.age");
    drop((store, record));
    aterm_secrets::fsx::write_atomic(&path, &replacement, 0o600).unwrap();
    assert!(recover(&env, &request, &reference).is_err());
    assert!(
        recover_pending(&env, &request).is_err(),
        "a missing outer checkpoint must not turn public encryption into producer authority"
    );
    let mut forged_reference = reference.clone();
    forged_reference.issuance_digest =
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&replacement));
    assert!(
        recover(&env, &request, &forged_reference).is_err(),
        "updating a reference digest cannot forge the machine producer MAC"
    );
    aterm_secrets::fsx::write_atomic(&path, &original, 0o600).unwrap();
    assert!(
        recover(&env, &request, &reference).is_ok(),
        "CONTROL: original issuance remains recoverable"
    );
}

#[test]
fn retained_response_keeps_stage_unchanged_and_refuses_replaced_parent() {
    let (_root, env, stage, request) = fixture(true);
    let reference = fixture_complete(prepared(&env, &stage, &request));
    let grant = recover(&env, &request, &reference).unwrap();
    with_retained_response(
        &env,
        &request,
        &grant,
        |identity, bytes, witness, _, predecessor| {
            assert_eq!(*identity, Identity::from_native("account", "user"));
            assert_eq!(bytes, CANARY);
            assert!(
                predecessor.is_none(),
                "legacy response has no predecessor authority"
            );
            assert!(witness.matches_auth_file(&stage));
            assert!(
                codex_app_login::AuthLock::try_take(&stage)
                    .unwrap()
                    .is_none()
            );
            Ok(())
        },
    )
    .unwrap();
    assert!(
        !stage.exists(),
        "observing a response does not restore credentials"
    );
    aterm_secrets::fsx::write_atomic(&stage, b"different-native-document", 0o600).unwrap();
    with_retained_response(&env, &request, &grant, |_, bytes, _, _, _| {
        assert_eq!(bytes, CANARY);
        assert_eq!(std::fs::read(&stage).unwrap(), b"different-native-document");
        Ok(())
    })
    .unwrap();
    assert_eq!(std::fs::read(&stage).unwrap(), b"different-native-document");
    std::fs::remove_file(&stage).unwrap();
    let parent = stage.parent().unwrap();
    let held = parent.with_extension("old");
    std::fs::rename(parent, &held).unwrap();
    aterm_secrets::fsx::ensure_private_dir(parent).unwrap();
    assert!(with_retained_response(&env, &request, &grant, |_, _, _, _, _| Ok(())).is_err());
    assert!(!stage.exists());
}

#[test]
fn failed_publication_retains_the_completed_response_for_owned_retry() {
    let (_root, env, stage, request) = fixture(true);
    let mut pending = prepared(&env, &stage, &request);
    pending.begin_exchange().unwrap();
    let directory = pending.store.directory.clone();
    let held = directory.with_extension("held");
    std::fs::rename(&directory, &held).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&directory).unwrap();
    pending.capture_response(
        CANARY,
        Some(Identity::from_native("account", "user")),
        Some("fixture-code-owner".into()),
    );
    let CaptureOutcome::Pending(failure) = pending.settle_response() else {
        panic!("replacement custody must refuse publication")
    };
    let bytes = decoded_document(
        failure.pending.record.response.as_ref().unwrap(),
        MAX_DOCUMENT,
    )
    .unwrap();
    assert_eq!(bytes.as_slice(), CANARY);
    assert!(!format!("{failure:?}").contains("only-a-test"));
    std::fs::remove_dir(&directory).unwrap();
    std::fs::rename(&held, &directory).unwrap();
    let CaptureOutcome::Verified(grant) = failure.retry() else {
        panic!("restored custody must settle exact response")
    };
    assert!(recover(&env, &request, &grant.reference()).is_ok());
}

#[test]
fn unsupported_response_is_encrypted_without_verified_grant_or_stage_publication() {
    let (_root, env, stage, request) = fixture(true);
    let mut pending = prepared(&env, &stage, &request);
    pending.begin_exchange().unwrap();
    let document = vec![b'x'; MAX_DOCUMENT + 1];
    assert!(matches!(
        pending.retain_unverified(&document),
        CaptureOutcome::Unverified(_)
    ));
    let (_, record) = Store::open(&env, &request.id, false).unwrap().unwrap();
    let record = record.unwrap();
    assert_eq!(record.phase, Phase::UnverifiedResponse);
    assert!(record.response.as_ref().unwrap().identity.is_none());
    assert_eq!(
        decoded_document(record.response.as_ref().unwrap(), MAX_RETAINED_DOCUMENT)
            .unwrap()
            .as_slice(),
        document
    );
    assert!(!stage.exists());
}

#[test]
fn fresh_baseline_retry_keeps_exact_ciphertext_and_refuses_changed_credentials() {
    let (_root, env, stage, request) = fixture(true);
    let reference = fixture_complete(prepared(&env, &stage, &request));
    let binding = GrantBinding {
        request,
        reference,
        generation: 17,
    };
    let identity = Identity::from_native("account", "user");
    // Test-only authority injection; production must use the real publication
    // permit and verified live-consumer lease to construct this guard.
    let guard = FreshActivationGuard {
        binding: &binding,
        identity: &identity,
        document: CANARY,
    };
    let first = initialize_revision(&env, &guard).unwrap();
    assert!(first.is_fresh_activation());
    assert!(first.expected().is_none());
    assert_eq!(first.binding(), &binding);
    assert_eq!(first.next().revision.generation, 1);
    assert!(first.next().pending.is_none());
    let (owner, _) = Store::location(&env, &binding.request.id).unwrap();
    let path = owner.join("refresh/candidate.age");
    let ciphertext = std::fs::read(&path).unwrap();
    assert!(
        !ciphertext
            .windows(b"only-a-test-refresh".len())
            .any(|part| part == b"only-a-test-refresh")
    );
    assert_eq!(
        first.next().journal_digest,
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&ciphertext))
    );
    let repeated = initialize_revision(&env, &guard).unwrap();
    assert!(first.next() == repeated.next());
    assert_eq!(std::fs::read(&path).unwrap(), ciphertext);
    let changed = FreshActivationGuard {
        binding: &binding,
        identity: &identity,
        document: b"different credential document",
    };
    assert!(initialize_revision(&env, &changed).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), ciphertext);
    let mut next_binding = binding.clone();
    next_binding.generation += 1;
    let retry_guard = FreshActivationGuard {
        binding: &next_binding,
        identity: &identity,
        document: CANARY,
    };
    let rebound = initialize_revision(&env, &retry_guard).unwrap();
    assert_eq!(rebound.binding(), &next_binding);
    assert_eq!(rebound.next().revision.generation, 1);
    assert!(
        initialize_revision(&env, &guard).is_err(),
        "old publication generation cannot return"
    );
}

#[test]
fn no_dispatch_requires_live_cleanup_and_authenticated_exact_request() {
    let (_world, env, stage, mut request) = fixture(true);
    let mut uncertain = prepared(&env, &stage, &request);
    uncertain.begin_exchange().unwrap();
    drop(uncertain);
    assert!(recover_not_dispatched(&env, &request).unwrap().is_none());
    assert!(prepare(&env, &request, &stage, &"d".repeat(64)).is_err());
    request.id = "e".repeat(32);
    let mut capture = prepared(&env, &stage, &request);
    capture.begin_exchange().unwrap();
    let CaptureOutcome::NotDispatched(receipt) = capture.settle_not_dispatched() else {
        panic!("actual pre-send branch must settle")
    };
    assert!(receipt.matches_request(&request));
    let mut changed = request.clone();
    changed.caller = "f".repeat(64);
    assert!(!receipt.matches_request(&changed));
    assert!(recover_not_dispatched(&env, &changed).is_err());
    let (owner, _) = Store::open(&env, &request.id, false).unwrap().unwrap();
    let before = owner.journal.sealed_digest().unwrap();
    drop(owner);
    assert!(recover_not_dispatched(&env, &request).unwrap().is_some());
    let (owner, _) = Store::open(&env, &request.id, false).unwrap().unwrap();
    assert_eq!(
        owner.journal.sealed_digest().unwrap(),
        before,
        "durable recovery must flush the exact cipher, not reencrypt evidence"
    );
    drop(owner);
    assert!(recover_pending(&env, &request).is_err());
    assert!(!stage.exists());
    let (mut store, record) = Store::open(&env, &request.id, false).unwrap().unwrap();
    let mut record = record.unwrap();
    record.no_dispatch_seal = Some("0".repeat(64));
    // Public-recipient encryption of a well-formed record cannot assert no POST.
    store.write(&record).unwrap();
    drop(store);
    assert!(recover_not_dispatched(&env, &request).is_err());
}

#[test]
fn failed_no_dispatch_cleanup_keeps_owned_evidence_until_exact_retry() {
    let (_world, env, stage, request) = fixture(true);
    let mut capture = prepared(&env, &stage, &request);
    capture.begin_exchange().unwrap();
    let directory = capture.store.directory.clone();
    let held = directory.with_extension("held");
    std::fs::rename(&directory, &held).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&directory).unwrap();
    let CaptureOutcome::Pending(failure) = capture.settle_not_dispatched() else {
        panic!("failed cleanup must remain owned")
    };
    assert!(failure.pending.no_dispatch_pending);
    assert!(failure.pending.record.response.is_none());
    std::fs::remove_dir(&directory).unwrap();
    std::fs::rename(&held, &directory).unwrap();
    let CaptureOutcome::NotDispatched(receipt) = failure.retry() else {
        panic!("restored cleanup must settle without a provider request")
    };
    assert!(receipt.matches_request(&request));
    assert!(recover_not_dispatched(&env, &request).unwrap().is_some());
    assert!(!stage.exists());
}

#[test]
fn durable_no_dispatch_recovery_refuses_only_exact_running_operation_without_grant() {
    let (_world, env, stage, request) = fixture(true);
    let store = env
        .accounts_store_for(super::super::accounts::Cli::Codex)
        .unwrap();
    store
        .transact_logins(env.now, &mut None, |_, logins| {
            logins.enrollment.admit_native(&request)?;
            Ok(Vec::new())
        })
        .unwrap();
    let mut capture = prepared(&env, &stage, &request);
    capture.begin_exchange().unwrap();
    // A live owner claim is unavailable, and Attempted alone never becomes a
    // terminal refusal. The owner unit above separately covers a dropped claim.
    assert!(codex_enrollment::settle_not_dispatched(&env, &request).is_err());
    assert_eq!(
        store
            .load_logins(env.now)
            .unwrap()
            .0
            .enrollment
            .lookup(&request)
            .unwrap()
            .unwrap()
            .phase,
        codex_enrollment::Phase::Running
    );
    let CaptureOutcome::NotDispatched(_) = capture.settle_not_dispatched() else {
        panic!("cleanup failed")
    };
    // Reconstruct from durable authenticated evidence, without a live Control.
    codex_enrollment::settle_not_dispatched(&env, &request).unwrap();
    codex_enrollment::settle_not_dispatched(&env, &request).unwrap();
    let (logins, warning) = store.load_logins(env.now).unwrap();
    assert!(warning.is_none());
    let status = logins.enrollment.lookup(&request).unwrap().unwrap();
    assert_eq!(status.phase, codex_enrollment::Phase::Refused);
    assert!(status.generation.is_none());
    assert!(logins.enrollment.checkpoint_reference(&request).is_err());
    assert!(!stage.exists());
}

#[test]
fn rejected_response_requires_sender_mac_and_exact_immutable_response_context() {
    // Synthetic producer occurrence isolates encrypted-store authentication;
    // the real HTTP producer and Scope gates are exercised by app-login tests.
    let (_world, env, stage, request) = fixture(true);
    let mut capture = prepared(&env, &stage, &request);
    capture.begin_exchange().unwrap();
    capture.capture_rejection(400, br#"{"error":"invalid_grant"}"#);
    let CaptureOutcome::Rejected(rejection) = capture.settle_response() else {
        panic!("fixture failed")
    };
    assert!(rejection.matches_request(&request));
    assert!(recover_rejection(&env, &request).unwrap().is_some());
    assert!(recover_not_dispatched(&env, &request).unwrap().is_none());
    assert!(recover_pending(&env, &request).is_err());
    let (mut store, record) = Store::open(&env, &request.id, false).unwrap().unwrap();
    let original = SecretBytes::new(aterm_json::to_vec(&record.unwrap()).unwrap());
    for change in 0..6 {
        let mut record: Record = aterm_json::from_slice(original.as_slice()).unwrap();
        let mut expected = request.clone();
        match change {
            0 => {
                let body = br#"{"error":"invalid_grant","error_description":"changed"}"#;
                record.response.as_mut().unwrap().document =
                    aterm_codec::base64::encode(body).unwrap();
                record.rejection.as_mut().unwrap().body_digest =
                    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(body));
            }
            1 => {
                record.request.caller = "f".repeat(64);
                expected = record.request.clone();
            }
            2 => record.source_fence = "f".repeat(64),
            3 => record.stage_inode += 1,
            4 => record.capability = "f".repeat(64),
            _ => record.rejection.as_mut().unwrap().producer_mac = Some("0".repeat(64)),
        }
        // Public-recipient encryption can create this well-formed ciphertext,
        // but cannot make it an authenticated provider-rejection occurrence.
        store.write(&record).unwrap();
        drop(store);
        assert!(recover_rejection(&env, &expected).is_err());
        store = Store::open(&env, &request.id, false).unwrap().unwrap().0;
    }
    let mut record: Record = aterm_json::from_slice(original.as_slice()).unwrap();
    record.schema = 2;
    record.phase = Phase::NotDispatched;
    record.response = None;
    record.no_dispatch_seal = record.rejection.take().unwrap().producer_mac;
    // Even genuine provider evidence cannot bypass retirement through the
    // distinct no-send protocol, whose terminal semantics are different.
    store.write(&record).unwrap();
    drop(store);
    assert!(recover_not_dispatched(&env, &request).is_err());
    assert!(!stage.exists());
}

#[test]
fn raw_unverified_rejection_text_cannot_promote_itself_to_provider_proof() {
    let (_world, env, stage, request) = fixture(true);
    let mut capture = prepared(&env, &stage, &request);
    capture.begin_exchange().unwrap();
    assert!(matches!(
        capture.retain_unverified(br#"{"error":"invalid_grant"}"#),
        CaptureOutcome::Unverified(_)
    ));
    assert!(recover_rejection(&env, &request).unwrap().is_none());
    let (mut store, record) = Store::open(&env, &request.id, false).unwrap().unwrap();
    let mut record = record.unwrap();
    record.schema = 3;
    record.phase = Phase::RejectedResponse;
    record.rejection = Some(RejectionEvidence {
        status: 400,
        body_digest: aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
            br#"{"error":"invalid_grant"}"#,
        )),
        producer_mac: Some("0".repeat(64)),
        retirement_seal: None,
    });
    store.write(&record).unwrap();
    drop(store);
    assert!(recover_rejection(&env, &request).is_err());
}

#[test]
fn failed_rejection_write_keeps_raw_response_for_owned_retry_only() {
    let (_world, env, stage, request) = fixture(true);
    let mut capture = prepared(&env, &stage, &request);
    capture.begin_exchange().unwrap();
    let body = br#"{"error":"invalid_grant","error_description":"synthetic-only"}"#;
    capture.capture_rejection(400, body);
    let directory = capture.store.directory.clone();
    let held = directory.with_extension("held");
    std::fs::rename(&directory, &held).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&directory).unwrap();
    let CaptureOutcome::Pending(failure) = capture.settle_response() else {
        panic!("must retain custody")
    };
    assert_eq!(
        decoded_document(
            failure.pending.record.response.as_ref().unwrap(),
            MAX_DOCUMENT
        )
        .unwrap()
        .as_slice(),
        body
    );
    assert!(!failure.pending.no_dispatch_pending);
    std::fs::remove_dir(&directory).unwrap();
    std::fs::rename(&held, &directory).unwrap();
    let CaptureOutcome::Rejected(rejection) = failure.retry() else {
        panic!("must settle response only")
    };
    assert!(rejection.matches_request(&request));
    assert!(recover_rejection(&env, &request).unwrap().is_some());
    assert!(!stage.exists());
}

#[test]
fn provider_mac_alone_cannot_forge_durable_resource_retirement() {
    let (_world, env, stage, request) = fixture(true);
    let mut capture = prepared(&env, &stage, &request);
    capture.begin_exchange().unwrap();
    capture.capture_rejection(400, br#"{"error":"invalid_grant"}"#);
    assert!(matches!(
        capture.settle_response(),
        CaptureOutcome::Rejected(_)
    ));
    assert!(
        recover_rejection_retirement(&env, &request)
            .unwrap()
            .is_none()
    );
    let (mut store, record) = Store::open(&env, &request.id, false).unwrap().unwrap();
    let mut record = record.unwrap();
    record.schema = 4;
    let evidence = record.rejection.as_mut().unwrap();
    evidence.retirement_seal = evidence.producer_mac.clone();
    store.write(&record).unwrap();
    drop(store);
    assert!(
        recover_rejection(&env, &request).unwrap().is_some(),
        "provider fact is still authentic"
    );
    assert!(
        recover_rejection_retirement(&env, &request).is_err(),
        "provider fact is not resource authority"
    );
    assert!(codex_enrollment::settle_retired_rejection(&env, &request).is_err());
    let accounts = env
        .accounts_store_for(super::super::accounts::Cli::Codex)
        .unwrap();
    assert!(!accounts.logins_file().exists());
}

#[test]
fn public_recipient_cannot_replace_authenticated_stage_predecessor() {
    // Storage-only producer injection; the HTTP controls separately establish
    // that production obtains this value under the real pre-POST AuthLock.
    let (_root, env, stage, request) = fixture(true);
    let mut pending = prepared(&env, &stage, &request);
    pending.begin_exchange().unwrap();
    pending.capture_response(
        CANARY,
        Some(Identity::from_native("account", "user")),
        Some("fixture-owner".into()),
    );
    pending.record.schema = 5;
    pending.record.response.as_mut().unwrap().predecessor = Some(StagePredecessor {
        digest: Some("a".repeat(64)),
    });
    let CaptureOutcome::Verified(grant) = pending.settle_response() else {
        panic!("verified storage fixture")
    };
    let reference = grant.reference();
    let (store, record) = Store::open(&env, &request.id, false).unwrap().unwrap();
    let mut record = record.unwrap();
    let path = store.directory.join("candidate.age");
    let original = std::fs::read(&path).unwrap();
    record.response.as_mut().unwrap().predecessor = Some(StagePredecessor {
        digest: Some("b".repeat(64)),
    });
    let bytes = SecretBytes::new(aterm_json::to_vec(&record).unwrap());
    let replacement = age::encrypt(&[store.identity.recipient()], bytes.as_slice()).unwrap();
    drop((store, record));
    aterm_secrets::fsx::write_atomic(&path, &replacement, 0o600).unwrap();
    assert!(recover_pending(&env, &request).is_err());
    assert!(recover(&env, &request, &reference).is_err());
    aterm_secrets::fsx::write_atomic(&path, &original, 0o600).unwrap();
    assert!(recover(&env, &request, &reference).is_ok());
}
