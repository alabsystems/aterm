// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts;

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
    let directory = pending.store.held().unwrap().directory.clone();
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
    let directory = capture.store.held().unwrap().directory.clone();
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
    // Historical issuance was prepared before native canonical admission.
    let mut capture = prepared(&env, &stage, &request);
    let store = env
        .accounts_store_for(super::super::accounts::Cli::Codex)
        .unwrap();
    store
        .transact_logins(env.now, &mut None, |_, logins| {
            logins
                .enrollment
                .admit_historical_native_fixture(&request)?;
            Ok(Vec::new())
        })
        .unwrap();
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
    let directory = capture.store.held().unwrap().directory.clone();
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

#[test]
fn parked_browser_preparation_releases_owner_and_refuses_changed_ciphertext() {
    let (_root, env, stage, request) = fixture(true);
    let mut capture = prepared(&env, &stage, &request);
    capture.store.park(&env).unwrap();
    // This is an actual second claim, not a simulated lock state.
    let (mut peer, record) = Store::open(&env, &request.id, false).unwrap().unwrap();
    let mut record = record.unwrap();
    record.source_fence = "e".repeat(64);
    peer.write(&record).unwrap();
    drop(peer);
    assert!(capture.begin_exchange().is_err());
    let (_, current) = Store::open(&env, &request.id, false).unwrap().unwrap();
    assert_eq!(current.unwrap().phase, Phase::Prepared);
}

#[test]
fn exact_parked_preparation_can_reopen_once_before_actual_exchange() {
    let (_root, env, stage, request) = fixture(true);
    let mut capture = prepared(&env, &stage, &request);
    capture.store.park(&env).unwrap();
    let peer = Store::open(&env, &request.id, false).unwrap().unwrap();
    drop(peer);
    capture.begin_exchange().unwrap();
    assert!(capture.begin_exchange().is_err());
    assert!(Store::open(&env, &request.id, false).is_err());
    drop(capture);
    let (_, record) = Store::open(&env, &request.id, false).unwrap().unwrap();
    assert_eq!(record.unwrap().phase, Phase::Attempted);
}

#[test]
fn legacy_record_schemas_one_through_five_keep_exact_encoding_and_origin_domains() {
    // These local historical shapes test bytes, not publication authority.
    #[derive(serde::Serialize)]
    struct OldResponse<'a> {
        document: &'a str,
        identity: &'a Option<Identity>,
        login_owner: &'a Option<String>,
        producer_seal: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        predecessor: &'a Option<StagePredecessor>,
    }
    #[derive(serde::Serialize)]
    struct OldRecord<'a> {
        schema: u32,
        request: &'a codex_enrollment::Request,
        stage: &'a Path,
        stage_device: u64,
        stage_inode: u64,
        source_fence: &'a str,
        capability: &'a str,
        phase: &'a Phase,
        response: Option<OldResponse<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        no_dispatch_seal: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rejection: &'a Option<RejectionEvidence>,
    }
    let (_root, env, stage, request) = fixture(true);
    let reference = fixture_complete(prepared(&env, &stage, &request));
    let (store, mut record) = open_authenticated(&env, &request, &reference).unwrap();
    for schema in [1, 5, 3, 4, 2] {
        record.schema = schema;
        if schema <= 1 || schema == 5 {
            record.phase = Phase::VerifiedResponse;
            record.rejection = None;
            record.no_dispatch_seal = None;
            let response = record.response.as_mut().unwrap();
            response.predecessor = (schema == 5).then_some(StagePredecessor { digest: None });
            let identity = response.identity.as_ref().unwrap();
            let owner = response.login_owner.as_ref().unwrap();
            let legacy = (
                &record.request,
                &record.stage,
                record.stage_device,
                record.stage_inode,
                &record.source_fence,
                &record.capability,
                identity,
                owner,
                &response.document,
            );
            let body = if schema == 5 {
                aterm_json::to_vec(&(
                    "aterm/codex-code-response-origin/v2",
                    legacy,
                    response.predecessor.as_ref().unwrap(),
                ))
                .unwrap()
            } else {
                aterm_json::to_vec(&(
                    "aterm/codex-code-response-origin/v1",
                    &record.request,
                    &record.stage,
                    record.stage_device,
                    record.stage_inode,
                    &record.source_fence,
                    &record.capability,
                    identity,
                    owner,
                    &response.document,
                ))
                .unwrap()
            };
            let key: &[u8] = if schema == 5 {
                b"aterm/codex-code-response-origin/key/v2\0"
            } else {
                b"aterm/codex-code-response-origin/key/v1\0"
            };
            assert_eq!(
                store.origin_seal(&record).unwrap(),
                machine_mac(&store.identity, key, &body).unwrap()
            );
        } else if schema == 2 {
            record.phase = Phase::NotDispatched;
            record.response = None;
            record.rejection = None;
            record.no_dispatch_seal = Some("a".repeat(64));
        } else {
            record.phase = Phase::RejectedResponse;
            record.no_dispatch_seal = None;
            let response = record.response.as_mut().unwrap();
            response.identity = None;
            response.login_owner = None;
            response.producer_seal = None;
            response.predecessor = None;
            record.rejection = Some(RejectionEvidence {
                status: 400,
                body_digest: "b".repeat(64),
                producer_mac: Some("c".repeat(64)),
                retirement_seal: (schema == 4).then(|| "d".repeat(64)),
            });
        }
        let historical = OldRecord {
            schema: record.schema,
            request: &record.request,
            stage: &record.stage,
            stage_device: record.stage_device,
            stage_inode: record.stage_inode,
            source_fence: &record.source_fence,
            capability: &record.capability,
            phase: &record.phase,
            response: record.response.as_ref().map(|response| OldResponse {
                document: &response.document,
                identity: &response.identity,
                login_owner: &response.login_owner,
                producer_seal: &response.producer_seal,
                predecessor: &response.predecessor,
            }),
            no_dispatch_seal: &record.no_dispatch_seal,
            rejection: &record.rejection,
        };
        assert_eq!(
            aterm_json::to_vec(&record).unwrap(),
            aterm_json::to_vec(&historical).unwrap(),
            "old schema {schema} bytes changed"
        );
    }
}

#[test]
fn native_row_issuer_binds_full_staging_and_exact_publication_occurrence() {
    let (_world, env, stage, request) = fixture(true);
    let logins = accounts::Logins::default();
    let raw = accounts::Roster::parse_for(
        accounts::Cli::Codex,
        "[[account]]\nlabel = 'fixture'\nemail = 'fixture@example.test'\nclass = 'dedicated'\n",
    )
    .unwrap();
    let directory = std::fs::canonicalize(stage.parent().unwrap()).unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    store.ensure().unwrap();
    let store_directory = std::fs::canonicalize(&store.dir).unwrap();
    let staging = codex_enrollment::Staging {
        native_row: Some(
            super::super::codex_native_row::NativeRowFence::capture(&raw, &logins, &request.label)
                .unwrap(),
        ),
        request: request.clone(),
        store: store_directory.clone(),
        directory: directory.clone(),
        store_identity: Some(
            codex_enrollment::DirectoryIdentity::capture(&store_directory).unwrap(),
        ),
        directory_identity: Some(codex_enrollment::DirectoryIdentity::capture(&directory).unwrap()),
        parent: accounts::RenewalParent::capture(&logins, &request.label),
        workflow: 1,
        policy: "d".repeat(64),
        selection: None,
    };
    // Synthetic issuer injection tests the real authenticated encrypted owner
    // path, not provider provenance or a production admission constructor.
    let mut pending = prepared(&env, &stage, &request);
    pending.begin_exchange().unwrap();
    pending.capture_response(
        CANARY,
        Some(Identity::from_native("account", "user")),
        Some("test-owner".into()),
    );
    pending.record.schema = 6;
    let response = pending.record.response.as_mut().unwrap();
    response.predecessor = Some(StagePredecessor { digest: None });
    response.native_row = Some(staging.clone());
    let CaptureOutcome::Verified(fresh) = pending.settle_response() else {
        panic!("fixture failed")
    };
    let issuer = row_publication_issuer(&env, &request, &fresh).unwrap();
    let binding = GrantBinding {
        request: request.clone(),
        reference: fresh.reference(),
        generation: 7,
    };
    let fingerprint = "e".repeat(64);
    let seal = issuer
        .authenticate_receipt(&binding, &staging, &fingerprint)
        .unwrap();
    #[derive(Serialize)]
    struct Receipt<'a> {
        schema: u32,
        binding: &'a GrantBinding,
        staging: &'a codex_enrollment::Staging,
        binding_digest: &'a str,
        mac: &'a str,
    }
    let data = |binding: &GrantBinding, staging: &codex_enrollment::Staging, fingerprint: &str| {
        aterm_json::from_str::<super::super::codex_native_row::PublicationReceiptData>(
            &aterm_json::to_string(&Receipt {
                schema: 1,
                binding,
                staging,
                binding_digest: fingerprint,
                mac: &seal,
            })
            .unwrap(),
        )
        .unwrap()
    };
    issuer
        .verify_publication(&data(&binding, &staging, &fingerprint))
        .unwrap();
    let mut forged_binding = binding.clone();
    forged_binding.generation += 1;
    assert!(
        issuer
            .verify_publication(&data(&forged_binding, &staging, &fingerprint))
            .is_err()
    );
    assert!(
        issuer
            .verify_publication(&data(&binding, &staging, &"f".repeat(64)))
            .is_err()
    );
    let mut changed = staging.clone();
    changed.selection = Some("changed-selection".into());
    assert!(
        issuer
            .authenticate_receipt(&binding, &changed, &fingerprint)
            .is_err()
    );
    assert!(
        issuer
            .verify_publication(&data(&binding, &changed, &fingerprint))
            .is_err()
    );
    let mut changed_binding = binding.clone();
    changed_binding.generation = 8;
    assert_ne!(
        seal,
        issuer
            .authenticate_receipt(&changed_binding, &staging, &fingerprint)
            .unwrap()
    );
    assert_ne!(
        seal,
        issuer
            .authenticate_receipt(&binding, &staging, &"f".repeat(64))
            .unwrap()
    );
    // Actual writer/channel controls with synthetic issuer input; this does
    // not qualify source/physical admission or live OAuth.
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let roster = _world.path().join("channel-roster.toml");
    std::fs::write(
        &roster,
        "[[account]]\nlabel='fixture'\nemail='fixture@example.test'\nclass='dedicated'\n",
    )
    .unwrap();
    store
        .transact_logins(env.now, &mut None, |_, logins| {
            logins.enrollment.admit_native(&request)?;
            Ok(Vec::new())
        })
        .unwrap();
    store
        .transact_logins(env.now, &mut None, |_, logins| {
            logins.enrollment.bind_staging(staging.clone())?;
            logins.enrollment.attach_grant(&request, &fresh)?;
            Ok(Vec::new())
        })
        .unwrap();
    let original = std::fs::read(store.logins_file()).unwrap();
    let detached = std::cell::RefCell::new(None);
    let edit =
        |_: &accounts::Roster, state: &mut accounts::State, logins: &mut accounts::Logins| {
            let events = accounts::note_renewed(
                state,
                logins,
                &request.label,
                None,
                directory.to_str().unwrap(),
                None,
                env.now,
            );
            logins.enrollment.publishing(&request)?;
            Ok(events)
        };
    assert!(
        store
            .transact_roster_logins_native_within(
                &[],
                &roster,
                env.now,
                &mut None,
                edit,
                |permit| {
                    *detached.borrow_mut() =
                        Some(super::super::codex_native_row::issue_publication(
                            &permit, &request, &fresh, &issuer,
                        )?);
                    Err("injected rollback".into())
                }
            )
            .is_err()
    );
    assert_eq!(std::fs::read(store.logins_file()).unwrap(), original);
    assert!(
        store
            .transact_roster_logins_native_within(
                &[],
                &roster,
                env.now,
                &mut None,
                edit,
                |permit| {
                    permit.attach_row_receipt(detached.borrow_mut().take().unwrap())?;
                    Ok(None)
                }
            )
            .is_err(),
        "foreign transaction stamp must refuse"
    );
    assert_eq!(std::fs::read(store.logins_file()).unwrap(), original);
    store
        .transact_roster_logins_native_within(&[], &roster, env.now, &mut None, edit, |permit| {
            permit.attach_row_receipt(super::super::codex_native_row::issue_publication(
                &permit, &request, &fresh, &issuer,
            )?)?;
            let duplicate = super::super::codex_native_row::issue_publication(
                &permit, &request, &fresh, &issuer,
            )?;
            assert!(
                permit.attach_row_receipt(duplicate).is_err(),
                "one-shot channel"
            );
            Ok(None)
        })
        .unwrap();
    let (published, _) = store.load_logins(env.now).unwrap();
    let actual = published
        .enrollment
        .published_grant(
            &request.label,
            published
                .get(&request.label)
                .unwrap()
                .last_success_generation,
        )
        .unwrap();
    issuer
        .verify_publication(published.enrollment.row_publication(&actual).unwrap())
        .unwrap();
    assert!(
        !stage.exists(),
        "receipt signing never publishes credentials"
    );
}

pub(super) fn with_two_native_rows_fixture(
    test: impl FnOnce(&Env, &accounts::Store, &Path, [NativeRowFixture; 2]),
) {
    with_native_rows_at_attachment_cut(true, test)
}

pub(super) fn with_native_rows_at_attachment_cut(
    attach: bool,
    test: impl FnOnce(&Env, &accounts::Store, &Path, [NativeRowFixture; 2]),
) {
    let (world, mut env, _, _) = fixture(true);
    let config = world.path().join("aterm.toml");
    std::fs::write(&config, "").unwrap();
    env.config = Some(config);
    let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    store.ensure().unwrap();
    let old_a = world.path().join("old-a");
    let old_b = world.path().join("old-b");
    for path in [&old_a, &old_b] {
        aterm_secrets::fsx::ensure_private_dir(path).unwrap();
    }
    std::fs::write(&roster, format!(
        "[[account]]\nlabel='alpha'\nemail='alpha@example.test'\nclass='dedicated'\ndir={}\n[[account]]\nlabel='beta'\nemail='beta@example.test'\nclass='dedicated'\ndir={}\n",
        aterm_json::to_string(old_a.to_str().unwrap()).unwrap(),
        aterm_json::to_string(old_b.to_str().unwrap()).unwrap(),
    )).unwrap();
    let prepare_row = |label: &str, nonce: char| {
        let request = codex_enrollment::Request {
            id: nonce.to_string().repeat(32),
            caller: "b".repeat(64),
            label: label.into(),
            revision: "c".repeat(64),
        };
        store
            .transact_logins(env.now, &mut None, |_, logins| {
                logins.enrollment.admit_native(&request)?;
                Ok(Vec::new())
            })
            .unwrap();
        let directory = store.claim_staging_dir(label).unwrap();
        let held = store.acquire_bounded().unwrap().unwrap();
        store
            .transact_pool_logins_held(&held, &roster, env.now, &mut None, |_, _, logins| {
                accounts::admit_workflow(logins, label)?;
                Ok(Vec::new())
            })
            .unwrap();
        let snapshot = store
            .native_row_snapshot_held(&held, &roster, env.now)
            .unwrap();
        let records = snapshot.account_image();
        let parent = accounts::RenewalParent::capture(&records.logins, label);
        let workflow =
            accounts::current_workflow(records.raw_roster.row(label).unwrap(), &records.logins)
                .unwrap()
                .value();
        let occurrence =
            super::super::codex_native_row::NativeRowOccurrence::capture(snapshot, &request)
                .unwrap();
        let staging = codex_enrollment::Staging {
            native_row: Some(occurrence.fence().clone()),
            request: request.clone(),
            store: std::fs::canonicalize(&store.dir).unwrap(),
            directory: std::fs::canonicalize(&directory).unwrap(),
            store_identity: Some(codex_enrollment::DirectoryIdentity::capture(&store.dir).unwrap()),
            directory_identity: Some(
                codex_enrollment::DirectoryIdentity::capture(&directory).unwrap(),
            ),
            parent,
            workflow,
            policy: occurrence.policy().to_owned(),
            selection: None,
        };
        let admission = occurrence.finish(staging.clone()).unwrap();
        let artifact = std::cell::RefCell::new(None);
        store
            .transact_roster_logins_held(&held, &roster, env.now, &mut None, |_, _, logins| {
                *artifact.borrow_mut() = Some(
                    logins
                        .enrollment
                        .bind_staging(admission.staging().clone())?,
                );
                Ok(Vec::new())
            })
            .unwrap();
        drop(held);
        // Synthetic producer injection, real encrypted issuer and canonical
        // writer. This test proves row reconciliation, not provider identity,
        // independence, SourceV1, or browser/email acceptance.
        let claims = aterm_json::json!({"sub":label,"email":format!("{label}@example.test"),"exp":4_000_000_000u64,
            "https://api.openai.com/auth":{"chatgpt_account_id":label,"chatgpt_user_id":label}});
        let payload = aterm_codec::base64::encode(&aterm_json::to_vec(&claims).unwrap())
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_");
        let jwt = format!("e30.{payload}.c2ln");
        let document = aterm_json::to_string(&aterm_json::json!({"auth_mode":"chatgpt","last_refresh":env.now,
            "tokens":{"account_id":label,"id_token":jwt,"access_token":jwt,"refresh_token":format!("test-refresh-{label}"),"login_owner_id":format!("test-owner-{label}"),"oauth_provenance":{"issuer":codex_app_login::DEFAULT_ISSUER,"client_id":codex_app_login::CLIENT_ID}}})).unwrap();
        let mut pending = prepared(&env, &directory.join("auth.json"), &request);
        pending.begin_exchange().unwrap();
        pending.capture_response(
            document.as_bytes(),
            Some(Identity::from_native(label, label)),
            Some(format!("test-owner-{label}")),
        );
        pending.record.schema = 6;
        let response = pending.record.response.as_mut().unwrap();
        response.predecessor = Some(StagePredecessor { digest: None });
        response.native_row = Some(staging.clone());
        let CaptureOutcome::Verified(fresh) = pending.settle_response() else {
            panic!("fixture retention failed")
        };
        if attach {
            store
                .transact_logins(env.now, &mut None, |_, logins| {
                    logins.enrollment.attach_grant(&request, &fresh)?;
                    Ok(Vec::new())
                })
                .unwrap();
        }
        (request, staging, fresh, artifact.into_inner())
    };
    let rows = [prepare_row("alpha", 'a'), prepare_row("beta", 'e')];
    test(&env, &store, &roster, rows);
}

#[test]
fn two_native_row_publications_join_only_authenticated_other_row_changes() {
    with_two_native_rows_fixture(|env, store, roster, rows| {
        let [(a, stage_a, fresh_a, _), (b, stage_b, fresh_b, _)] = rows;
        let publish = |request: &codex_enrollment::Request,
                       staging: &codex_enrollment::Staging,
                       fresh: &RetainedGrant| {
            let held = store.acquire_bounded().unwrap().unwrap();
            super::super::codex_native_row::with_current_row(
                env,
                (store, &held),
                roster,
                env.now,
                request,
                fresh,
                |current| {
                    store.transact_roster_logins_native_held(
                        &held,
                        roster,
                        env.now,
                        &mut None,
                        |raw, state, logins| {
                            current.require_edit_image(raw, state, logins)?;
                            let events = accounts::note_renewed(
                                state,
                                logins,
                                &request.label,
                                None,
                                staging.directory.to_str().unwrap(),
                                None,
                                env.now,
                            );
                            logins.enrollment.publishing(request)?;
                            Ok(events)
                        },
                        |permit| {
                            current.attach_publication(&permit, fresh)?;
                            Ok(None)
                        },
                    )
                },
            )
        };
        publish(&a, &stage_a, &fresh_a).unwrap();
        let (after_a, _) = store.load_logins(env.now).unwrap();
        assert!(
            stage_b.parent.require_current(&after_a, &b.label).is_err(),
            "legacy absence guard stays strict"
        );
        let a_generation = after_a.get(&a.label).unwrap().last_success_generation;
        store
            .transact_logins(env.now, &mut None, |_, logins| {
                logins.labels.get_mut(&a.label).unwrap().ended_at = Some(env.now);
                Ok(Vec::new())
            })
            .unwrap();
        let (changed, _) = store.load_logins(env.now).unwrap();
        assert_eq!(
            changed.get(&a.label).unwrap().last_success_generation,
            a_generation
        );
        let before = std::fs::read(store.logins_file()).unwrap();
        assert!(
            publish(&b, &stage_b, &fresh_b).is_err(),
            "same G/ref cannot authenticate metadata edits"
        );
        assert_eq!(std::fs::read(store.logins_file()).unwrap(), before);
        store
            .transact_logins(env.now, &mut None, |_, logins| {
                logins.labels.get_mut(&a.label).unwrap().ended_at = None;
                Ok(Vec::new())
            })
            .unwrap();
        publish(&b, &stage_b, &fresh_b).unwrap();
        let (complete, _) = store.load_logins(env.now).unwrap();
        assert_eq!(complete.generation_clock, 2);
        for (request, generation) in [(&a, 1), (&b, 2)] {
            assert_eq!(
                complete.enrollment.lookup(request).unwrap().unwrap().phase,
                codex_enrollment::Phase::Applied
            );
            assert_eq!(
                complete
                    .get(&request.label)
                    .unwrap()
                    .last_success_generation,
                generation
            );
        }
    });
}
