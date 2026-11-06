// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

const EMAIL: &str = "person@example.com";
const RENEWED: &[u8] = br#"{"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600,"scope":"coding","diagnostic":"not-native-token-data"}"#;

#[derive(Debug)]
struct Bound {
    context: RecoveryContext,
    source: Arc<dyn Authority>,
}
impl aterm_http::Guard for Bound {
    fn is_authorized(&self) -> bool {
        self.source.is_authorized()
    }
}
impl Authority for Bound {
    fn native_lease_clock(&self) -> Option<Arc<std::sync::atomic::AtomicU64>> {
        self.source.native_lease_clock()
    }
    fn receipt_current(&self) -> bool {
        self.source.receipt_current()
    }
    fn binding_current(&self) -> bool {
        self.source.binding_current()
    }
    fn recovery_context(&self) -> Result<RecoveryContext, String> {
        Ok(self.context.clone())
    }
}
fn bound(home: &Path, source: Arc<dyn Authority>) -> Arc<dyn Authority> {
    let store =
        accounts::Store::at_for(&home.join("harness"), crate::ai::connector::Cli::Kimi, home);
    Arc::new(Bound {
        context: RecoveryContext::new(&store, "one", login_drive::RecoveryKey([17; 32]), home),
        source,
    })
}

#[derive(Debug)]
struct SleptLease(Arc<std::sync::atomic::AtomicU64>);
impl aterm_http::Guard for SleptLease {
    fn is_authorized(&self) -> bool {
        true
    }
}
impl Authority for SleptLease {
    fn receipt_current(&self) -> bool {
        true
    }
    fn native_lease_clock(&self) -> Option<Arc<std::sync::atomic::AtomicU64>> {
        Some(self.0.clone())
    }
}

#[test]
fn sealed_completed_refresh_is_consumed_under_a_fresh_lease_without_replaying_http() {
    use std::os::unix::fs::MetadataExt as _;
    for case in 0..7 {
        let dir = stage(Region::Global);
        let path = credential_path(dir.path(), Region::Global);
        let before = std::fs::read(&path).unwrap();
        let target = Target {
            region: Region::Global,
            expected: EMAIL,
            on_rejected: None,
        };
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let authority = bound(dir.path(), Arc::new(SleptLease(clock.clone())));
        let result = coordinated_using(
            dir.path(),
            target,
            1000,
            deadline(),
            authority.clone(),
            |_, _| Err(NativeError::Expired),
            |_, _, _| {
                // Same actual lease/inode, but its original five-second clock has expired.
                clock.store(10 * 1_000_000_000, Ordering::Release);
                if case == 1 {
                    let lease = dir
                        .path()
                        .join(oauth_key(Region::Global))
                        .with_extension("lock");
                    std::fs::rename(&lease, lease.with_extension("retained")).unwrap();
                    std::fs::create_dir(&lease).unwrap();
                }
                Ok((200, aterm_secrets::SecretBytes::from(RENEWED)))
            },
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "stale lease cannot publish"
        );
        let context = authority.recovery_context().unwrap();
        let record = completion::Record::open(dir.path(), target, context.clone()).unwrap();
        let (status, received, body) = record
            .saved_response()
            .expect("exact completed response survived its owner");
        assert_eq!(status, 200);
        assert_eq!(body, RENEWED);
        let (candidate, _) = response_candidate(status, body, received).unwrap();
        let original_expiry =
            aterm_json::from_slice::<Value>(candidate.as_slice()).unwrap()["expires_at"]
                .as_i64()
                .unwrap();
        drop(record);
        if case == 1 {
            let lease = dir
                .path()
                .join(oauth_key(Region::Global))
                .with_extension("lock");
            std::fs::remove_dir(&lease).unwrap();
            std::fs::remove_dir(lease.with_extension("retained")).unwrap();
            aterm_secrets::fsx::write_atomic(&path, b"unrelated replacement", 0o600).unwrap();
        }
        if case == 2 {
            std::fs::write(dir.path().join("config.toml"), b"changed=true\n").unwrap();
        }
        if case == 4 {
            aterm_secrets::fsx::write_atomic(&path, candidate.as_slice(), 0o600).unwrap();
        }
        let mut consumer_staging = dir.path().to_owned();
        if case == 6 {
            consumer_staging = dir.path().join("new-admitted-staging");
            aterm_secrets::fsx::ensure_private_dir(&consumer_staging).unwrap();
            check_directory(&consumer_staging).expect("the newly admitted staging root is private");
            aterm_secrets::fsx::ensure_private_dir(&consumer_staging.join("oauth")).unwrap();
            aterm_secrets::fsx::ensure_private_dir(&consumer_staging.join("credentials")).unwrap();
            aterm_secrets::fsx::write_atomic(
                &consumer_staging.join("config.toml"),
                &std::fs::read(dir.path().join("config.toml")).unwrap(),
                0o600,
            )
            .unwrap();
            aterm_secrets::fsx::write_atomic(
                &credential_path(&consumer_staging, Region::Global),
                &before,
                0o600,
            )
            .unwrap();
        }
        let consumed_path = credential_path(&consumer_staging, Region::Global);
        let applied_inode = std::fs::metadata(&consumed_path).unwrap().ino();
        let credential_before_consumer = std::fs::read(&consumed_path).unwrap();
        let mut context = context;
        if case == 3 {
            context = context.with_binding([42; 32]);
        }
        let current: Arc<dyn Authority> = Arc::new(Bound {
            context,
            source: Arc::new(aterm_http::AlwaysAuthorized),
        });
        let result = coordinated_using(
            &consumer_staging,
            target,
            if case == 5 { original_expiry + 1 } else { 1001 },
            deadline(),
            current.clone(),
            |now, _| {
                verify_with(&consumer_staging, Region::Global, EMAIL, now, |_, token| {
                    assert_eq!(token, "rotated-access");
                    Ok((
                        200,
                        aterm_json::to_vec(
                            &aterm_json::json!({"email": EMAIL, "user_id":"canonical-full-user"}),
                        )
                        .unwrap(),
                    ))
                })
            },
            |_, _, _| panic!("a retained completion repeated its HTTP exchange"),
        );
        if matches!(case, 1..=3) {
            assert!(result.is_err());
            assert_eq!(
                std::fs::read(&consumed_path).unwrap(),
                credential_before_consumer
            );
            assert!(
                completion::Record::open(dir.path(), target, authority.recovery_context().unwrap())
                    .unwrap()
                    .saved_response()
                    .is_some()
            );
        } else {
            let outcome = match &result {
                Ok(Some(_)) => "proof",
                Ok(None) => "no-proof",
                Err(NativeError::Temporary) => "temporary",
                Err(NativeError::Expired) => "expired",
                Err(NativeError::Rejected) => "rejected",
                Err(NativeError::RefreshRejected) => "refresh-rejected",
                Err(NativeError::PreviouslyRejected) => "previously-rejected",
                Err(NativeError::Local(_)) => "local-refusal",
                Err(NativeError::Refused(_)) => "authority-refusal",
            };
            assert_eq!(
                std::fs::read(&consumed_path).unwrap(),
                candidate.as_slice(),
                "retained response consumer case={case}, result={outcome}"
            );
            if case == 4 {
                assert_eq!(
                    std::fs::metadata(&consumed_path).unwrap().ino(),
                    applied_inode,
                    "already applied means no rewrite"
                );
            }
            if case == 5 {
                assert!(result.is_err());
                let value: Value =
                    aterm_json::from_slice(&std::fs::read(&consumed_path).unwrap()).unwrap();
                assert_eq!(
                    value["expires_at"].as_i64(),
                    Some(original_expiry),
                    "recovery cannot rebase expires_in"
                );
            } else {
                assert!(result.unwrap().is_some());
            }
            let record = completion::Record::open(
                &consumer_staging,
                target,
                current.recovery_context().unwrap(),
            )
            .unwrap();
            assert!(record.saved_response().is_none(), "applied receipt retired");
        }
    }
}

#[test]
fn a_recovered_rejection_records_the_current_observation_before_native_publication() {
    for allow in [false, true] {
        let dir = stage(Region::Global);
        let path = credential_path(dir.path(), Region::Global);
        let before = std::fs::read(&path).unwrap();
        let target = Target {
            region: Region::Global,
            expected: EMAIL,
            on_rejected: None,
        };
        let slept = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let original = bound(dir.path(), Arc::new(SleptLease(slept.clone())));
        let result = coordinated_using(
            dir.path(),
            target,
            1000,
            deadline(),
            original.clone(),
            |_, _| Err(NativeError::Expired),
            |_, _, _| {
                slept.store(10_000_000_000, Ordering::Release);
                Ok((
                    400,
                    aterm_secrets::SecretBytes::from(br#"{"error":"invalid_grant"}"#.as_slice()),
                ))
            },
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let observed = std::cell::Cell::new(false);
        let observe = || {
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "native tombstone preceded durable observation"
            );
            observed.set(true);
            if allow {
                Ok(())
            } else {
                Err("synthetic observation unavailable".into())
            }
        };
        let target = Target {
            on_rejected: Some(&observe),
            ..target
        };
        let current = bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized));
        let result = coordinated_using(
            dir.path(),
            target,
            1001,
            deadline(),
            current,
            |_, _| panic!("recovered invalid_grant cannot dispatch identity HTTP"),
            |_, _, _| panic!("recovered invalid_grant cannot repeat refresh HTTP"),
        );
        assert!(observed.get());
        if allow {
            assert!(matches!(result, Err(NativeError::PreviouslyRejected)));
            assert_eq!(std::fs::read(&path).unwrap(), REVOKED);
        } else {
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let record =
            completion::Record::open(dir.path(), target, original.recovery_context().unwrap())
                .unwrap();
        assert_eq!(record.saved_response().is_none(), allow);
    }
}

#[test]
fn missing_custody_or_an_armed_unknown_refresh_never_dispatches_again() {
    let dir = stage(Region::Global);
    let target = Target {
        region: Region::Global,
        expected: EMAIL,
        on_rejected: None,
    };
    let unavailable = coordinated_using(
        dir.path(),
        target,
        1000,
        deadline(),
        Arc::new(aterm_http::AlwaysAuthorized),
        |_, _| panic!("unavailable custody reached HTTP"),
        |_, _, _| panic!("unavailable custody refreshed"),
    );
    assert!(unavailable.is_err());
    let first = coordinated_using(
        dir.path(),
        target,
        1000,
        deadline(),
        bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized)),
        |_, _| Err(NativeError::Expired),
        |_, _, _| Err(NativeError::Temporary),
    );
    assert!(first.is_err());
    let second = coordinated_using(
        dir.path(),
        target,
        1000,
        deadline(),
        bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized)),
        |_, _| panic!("an unknown exchange reached HTTP"),
        |_, _, _| panic!("an unknown exchange was replayed"),
    );
    assert!(second.is_err());
}

#[test]
fn cancellation_after_this_calls_arm_retires_only_its_proven_unstarted_marker() {
    use crate::harness::account_work::{Lifetime, Scope};
    #[derive(Debug)]
    struct PauseAfterArm {
        prepared: Arc<completion::Prepared>,
        lifetime: Lifetime,
        work: Scope,
    }
    impl aterm_http::Guard for PauseAfterArm {
        fn is_authorized(&self) -> bool {
            !self.work.cancelled() && aterm_http::Guard::is_authorized(self.prepared.as_ref())
        }
    }
    impl Authority for PauseAfterArm {
        fn receipt_current(&self) -> bool {
            self.prepared.receipt_current()
        }
        fn checkpoint_started(
            &self,
            config: [u8; 32],
            predecessor: [u8; 32],
        ) -> Result<(), String> {
            self.prepared.checkpoint_started(config, predecessor)?;
            drop(self.lifetime.pause().unwrap());
            Ok(())
        }
        fn checkpoint_unstarted(&self) -> Result<(), String> {
            self.prepared.checkpoint_unstarted()
        }
        fn checkpoint_dispatching(&self) -> Result<(), String> {
            self.prepared.checkpoint_dispatching()
        }
    }
    let dir = stage(Region::Global);
    let path = credential_path(dir.path(), Region::Global);
    let before = std::fs::read(&path).unwrap();
    let target = Target {
        region: Region::Global,
        expected: EMAIL,
        on_rejected: None,
    };
    let authority = bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized));
    let context = authority.recovery_context().unwrap();
    let record = completion::Record::open(dir.path(), target, context.clone()).unwrap();
    let lifetime = Lifetime::default();
    let guard = PauseAfterArm {
        prepared: Arc::new(completion::Prepared::new(record, authority.clone(), target)),
        work: lifetime.enter().unwrap(),
        lifetime,
    };
    let result = renew_using(dir.path(), target, 1000, deadline(), &guard, |_, _| {
        panic!("known cancellation before exchange dispatched HTTP")
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(guard);
    assert!(
        completion::Record::existing(dir.path(), target, context.clone())
            .unwrap()
            .is_none()
    );
    let exchanges = std::cell::Cell::new(0);
    let result = coordinated_using(
        dir.path(),
        target,
        1001,
        deadline(),
        authority.clone(),
        |now, _| {
            verify_with(dir.path(), Region::Global, EMAIL, now, |_, _| {
                Ok((
                    200,
                    aterm_json::to_vec(
                        &aterm_json::json!({"email":EMAIL,"user_id":"canonical-full-user"}),
                    )
                    .unwrap(),
                ))
            })
        },
        |_, _, _| {
            exchanges.set(exchanges.get() + 1);
            Ok((200, aterm_secrets::SecretBytes::from(RENEWED)))
        },
    );
    assert!(result.unwrap().is_some());
    assert_eq!(
        exchanges.get(),
        1,
        "a fresh invocation may begin after proven no-dispatch cleanup"
    );
    let mut record = completion::Record::open(dir.path(), target, context.clone()).unwrap();
    record.arm([1; 32], [2; 32]).unwrap();
    drop(record);
    let imported = completion::Record::existing(dir.path(), target, context.clone())
        .unwrap()
        .unwrap();
    let imported = completion::Prepared::new(imported, authority, target);
    assert!(
        imported.checkpoint_unstarted().is_err(),
        "an imported unknown marker has no no-dispatch receipt"
    );
    drop(imported);
    assert!(
        completion::Record::existing(dir.path(), target, context)
            .unwrap()
            .is_some()
    );
}

#[test]
fn authority_withdrawn_by_actual_arm_publication_does_not_leave_a_false_unknown_exchange() {
    #[derive(Debug)]
    struct WithdrawOnArm {
        path: std::path::PathBuf,
        observed: Arc<AtomicBool>,
    }
    impl aterm_http::Guard for WithdrawOnArm {
        fn is_authorized(&self) -> bool {
            self.receipt_current()
        }
    }
    impl Authority for WithdrawOnArm {
        fn receipt_current(&self) -> bool {
            let armed = self.path.exists();
            if armed {
                self.observed.store(true, Ordering::Release);
            }
            !armed
        }
    }
    let dir = stage(Region::Global);
    let path = credential_path(dir.path(), Region::Global);
    let before = std::fs::read(&path).unwrap();
    let target = Target {
        region: Region::Global,
        expected: EMAIL,
        on_rejected: None,
    };
    let fresh = bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized));
    let context = fresh.recovery_context().unwrap();
    let record = completion::Record::open(dir.path(), target, context.clone()).unwrap();
    let marker = record.ciphertext_path();
    drop(record);
    let observed = Arc::new(AtomicBool::new(false));
    let source = bound(
        dir.path(),
        Arc::new(WithdrawOnArm {
            path: marker.clone(),
            observed: observed.clone(),
        }),
    );
    let result = coordinated_using(
        dir.path(),
        target,
        1000,
        deadline(),
        source,
        |_, _| Err(NativeError::Expired),
        |_, _, _| panic!("post-arm authority withdrawal dispatched HTTP"),
    );
    assert!(result.is_err());
    assert!(
        observed.load(Ordering::Acquire),
        "the actual private arm must precede authority withdrawal"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        !marker.exists(),
        "successful own arm followed by failed binding check still proves no exchange"
    );
    let exchanges = std::cell::Cell::new(0);
    let result = coordinated_using(
        dir.path(),
        target,
        1001,
        deadline(),
        fresh,
        |now, _| {
            verify_with(dir.path(), Region::Global, EMAIL, now, |_, _| {
                Ok((
                    200,
                    aterm_json::to_vec(
                        &aterm_json::json!({"email": EMAIL,"user_id":"canonical-full-user"}),
                    )
                    .unwrap(),
                ))
            })
        },
        |_, _, _| {
            exchanges.set(exchanges.get() + 1);
            Ok((200, aterm_secrets::SecretBytes::from(RENEWED)))
        },
    );
    assert!(result.unwrap().is_some());
    assert_eq!(exchanges.get(), 1);
}

#[test]
#[ignore = "contacts the pinned Kimi token endpoint once with a synthetic grant; no account credentials or browser"]
fn live_direct_refresh_rejects_synthetic_grant_through_the_production_http_path() {
    let dir = stage(Region::Global);
    let path = credential_path(dir.path(), Region::Global);
    aterm_secrets::fsx::write_atomic(&path,
        br#"{"access_token":"aterm-synthetic-access","refresh_token":"aterm-synthetic-no-account-refresh-probe","token_type":"Bearer","expires_at":0}"#, 0o600).unwrap();
    let result = verify_or_refresh(
        dir.path(),
        Region::Global,
        EMAIL,
        1000,
        deadline(),
        bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized)),
    );
    assert!(
        matches!(result, Err(NativeError::RefreshRejected)),
        "the endpoint did not return a structured grant rejection: {}",
        result
            .err()
            .map_or("no rejection".into(), |error| error.to_string())
    );
    let value: Value = aterm_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(value["access_token"].as_str(), Some(""));
    assert_eq!(value["refresh_token"].as_str(), Some(""));
}

#[derive(Debug)]
struct Gate(AtomicBool);
impl aterm_http::Guard for Gate {
    fn is_authorized(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
impl Authority for Gate {
    fn receipt_current(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn checkpoint_started(&self, _: [u8; 32], _: [u8; 32]) -> Result<(), String> {
        Ok(())
    }
    fn checkpoint_completed(&self, _: u16, _: i64, _: &[u8]) -> Result<(), String> {
        Ok(())
    }
    fn checkpoint_applied(&self) -> Result<(), String> {
        Ok(())
    }
}
fn stage(region: Region) -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().unwrap();
    drop(proof_fixture(dir.path(), region, EMAIL));
    expire_pending(dir.path(), region).unwrap();
    dir
}
fn deadline() -> login_drive::Deadline<'static> {
    login_drive::Deadline::after(std::time::Duration::from_secs(60))
}

#[test]
fn retained_native_proof_retires_its_lease_and_counted_custody_without_an_admission_cycle() {
    use crate::harness::{account_work::Lifetime, browser_window::ProfileLock};
    let dir = stage(Region::Global);
    let mut proof = proof_fixture(dir.path(), Region::Global, EMAIL);
    let policy = Arc::new(AtomicBool::new(true));
    let current = policy.clone();
    let admission =
        login_drive::RecoveryAdmission::pool(login_drive::RecoveryKey([71; 32]), move |_| {
            if current.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("withdrawn synthetic authority".into())
            }
        });
    let projected = admission.authority();
    let lifetime = Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let store = accounts::Store::at_for(
        &dir.path().join("harness"),
        crate::ai::connector::Cli::Kimi,
        dir.path(),
    );
    let context = RecoveryContext::new(&store, "one", admission.key(), dir.path());
    let target = Target {
        region: Region::Global,
        expected: EMAIL,
        on_rejected: None,
    };
    let record = completion::Record::open(dir.path(), target, context.clone()).unwrap();
    let source: Arc<dyn Authority> = Arc::new(AdmissionGuard {
        admission: projected.clone(),
        recovery: context.clone(),
        work: Some(scope),
    });
    let prepared = Arc::new(completion::Prepared::new(record, source, target));
    let lease =
        native_lock::NativeLock::acquire(dir.path(), Region::Global, deadline(), prepared.clone())
            .unwrap();
    let guard = lease.guard();
    let lease_path = dir
        .path()
        .join(oauth_key(Region::Global))
        .with_extension("lock");
    assert!(
        lease_path.is_dir(),
        "the actual lease must exist before testing its retirement"
    );
    assert!(guard.receipt_current());
    policy.store(false, Ordering::Release);
    assert!(projected.current(None).is_err());
    assert!(
        !guard.receipt_current(),
        "projecting authority must preserve live revocation"
    );
    policy.store(true, Ordering::Release);
    proof.native_lock = Some(lease);
    admission
        .retain_kimi(proof, Arc::new(ProfileLock::claim(dir.path()).unwrap()))
        .unwrap();
    drop(guard);
    drop(prepared);
    drop(projected);
    let pause = lifetime.pause().unwrap();
    let pause = pause
        .try_wait()
        .expect_err("the actual retained proof still owns counted lease custody");
    let config = dir.path().join("config.toml");
    let original = std::fs::read(&config).unwrap();
    aterm_secrets::fsx::write_atomic(&config, b"changed=true\n", 0o600).unwrap();
    assert!(
        admission
            .kimi_principal(dir.path(), Region::Global, EMAIL)
            .is_err()
    );
    aterm_secrets::fsx::write_atomic(&config, &original, 0o600).unwrap();
    drop(admission);
    assert!(
        !lease_path.exists(),
        "lease cleanup completed before the counted authority retired"
    );
    drop(completion::Record::open(dir.path(), target, context).unwrap());
    assert!(
        pause.try_wait().is_ok(),
        "the populated admission slot must not retain its own proof in a cycle"
    );
}

#[test]
fn valid_native_observation_does_not_prepare_unavailable_new_completion_storage() {
    for (expired, held_empty) in [(false, false), (true, false), (false, true), (true, true)] {
        let dir = stage(Region::Global);
        if !expired {
            drop(proof_fixture(dir.path(), Region::Global, EMAIL));
        }
        let store = accounts::Store::at_for(
            &dir.path().join("harness"),
            crate::ai::connector::Cli::Kimi,
            dir.path(),
        );
        std::fs::create_dir_all(&store.dir).unwrap();
        let target = Target {
            region: Region::Global,
            expected: EMAIL,
            on_rejected: None,
        };
        let authority = bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized));
        let held = if held_empty {
            let record =
                completion::Record::open(dir.path(), target, authority.recovery_context().unwrap())
                    .unwrap();
            let directory = record.ciphertext_path().parent().unwrap().to_owned();
            drop(record);
            Some(crate::harness::browser_window::ProfileLock::claim(&directory).unwrap())
        } else {
            // The real sealed-store parent is unusable, while native files remain readable.
            std::fs::write(
                store.dir.join("pending-native-kimi-refresh"),
                b"not a directory",
            )
            .unwrap();
            None
        };
        let result = coordinated_using(
            dir.path(),
            target,
            1000,
            deadline(),
            authority,
            |now, _| {
                verify_with(dir.path(), Region::Global, EMAIL, now, |_, _| {
                    assert!(
                        !expired,
                        "expired native credentials cannot dispatch identity HTTP"
                    );
                    Ok((
                        200,
                        aterm_json::to_vec(
                            &aterm_json::json!({"email": EMAIL, "user_id": "canonical-full-user"}),
                        )
                        .unwrap(),
                    ))
                })
            },
            |_, _, _| panic!("unavailable protected custody dispatched HTTP"),
        );
        if expired {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_some());
        }
        drop(held);
    }
}

#[test]
fn refresh_reclaims_and_rereads_a_record_published_after_the_absence_observation() {
    for completed in [false, true] {
        let dir = stage(Region::Global);
        let path = credential_path(dir.path(), Region::Global);
        let before = std::fs::read(&path).unwrap();
        let config = std::fs::read(dir.path().join("config.toml")).unwrap();
        let target = Target {
            region: Region::Global,
            expected: EMAIL,
            on_rejected: None,
        };
        let authority = bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized));
        let context = authority.recovery_context().unwrap();
        let introduced = std::cell::Cell::new(false);
        let retained = std::cell::RefCell::new(None);
        let result = coordinated_using(
            dir.path(),
            target,
            1001,
            deadline(),
            authority.clone(),
            |now, _| {
                if !introduced.replace(true) {
                    // Another admitted owner publishes after this call's absence check,
                    // during its actual initial native read and before refresh preparation.
                    let mut other =
                        completion::Record::open(dir.path(), target, context.clone()).unwrap();
                    other
                        .arm(
                            aterm_digest::Sha256::digest(&config),
                            aterm_digest::Sha256::digest(&before),
                        )
                        .unwrap();
                    if completed {
                        other.completed(200, 1000, RENEWED).unwrap();
                    }
                    *retained.borrow_mut() = Some((
                        other.ciphertext_path(),
                        std::fs::read(other.ciphertext_path()).unwrap(),
                    ));
                    return Err(NativeError::Expired);
                }
                verify_with(dir.path(), Region::Global, EMAIL, now, |_, _| {
                    Ok((
                        200,
                        aterm_json::to_vec(
                            &aterm_json::json!({"email":EMAIL,"user_id":"canonical-full-user"}),
                        )
                        .unwrap(),
                    ))
                })
            },
            |_, _, _| panic!("an absence observation bypassed the newly claimed record"),
        );
        assert!(introduced.get());
        if completed {
            assert!(result.unwrap().is_some());
            let record = completion::Record::open(dir.path(), target, context).unwrap();
            assert!(record.saved_response().is_none());
        } else {
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            let retained = retained.borrow();
            let (marker, bytes) = retained.as_ref().unwrap();
            assert_eq!(
                std::fs::read(marker).unwrap(),
                *bytes,
                "a preexisting unknown exchange must stay untouched"
            );
        }
    }
}

#[test]
fn a_completed_rotation_survives_work_expiry_but_not_withdrawn_authority_or_files() {
    #[derive(Debug)]
    struct ResponseAuthority {
        binding: AtomicBool,
        work: crate::harness::account_work::Scope,
    }
    impl aterm_http::Guard for ResponseAuthority {
        fn is_authorized(&self) -> bool {
            self.binding.load(Ordering::SeqCst) && !self.work.cancelled()
        }
    }
    impl Authority for ResponseAuthority {
        fn receipt_current(&self) -> bool {
            self.binding.load(Ordering::SeqCst)
        }
    }
    for case in 0..4 {
        let dir = stage(Region::Global);
        let path = credential_path(dir.path(), Region::Global);
        let before = std::fs::read(&path).unwrap();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let gate = Arc::new(ResponseAuthority {
            binding: AtomicBool::new(true),
            work: lifetime.enter().unwrap(),
        });
        let base = aterm_uds::process::continuous_ns().expect("continuous clock on this host");
        let slept = std::cell::Cell::new(0);
        let continuous = || Some(base + slept.get());
        let budget = login_drive::Deadline::with_clocks(
            std::time::Duration::from_secs(3600),
            &Instant::now,
            &continuous,
        );
        let pause = std::cell::RefCell::new(None);
        let mut reads = 0;
        let result = coordinated_using(
            dir.path(),
            Target {
                region: Region::Global,
                expected: EMAIL,
                on_rejected: None,
            },
            1000,
            budget,
            bound(dir.path(), gate.clone()),
            |_, _| {
                reads += 1;
                Err(NativeError::Expired)
            },
            |_, _, _| {
                slept.set(7200 * 1_000_000_000);
                match case {
                    1 => gate.binding.store(false, Ordering::SeqCst),
                    2 => std::fs::write(dir.path().join("config.toml"), b"changed=true\n").unwrap(),
                    3 => *pause.borrow_mut() = lifetime.pause(),
                    _ => {}
                }
                Ok((200, aterm_secrets::SecretBytes::from(RENEWED)))
            },
        );
        assert!(
            result.is_err(),
            "expired work cannot return a publishable identity"
        );
        assert_eq!(
            reads, 2,
            "no identity request follows the completed exchange"
        );
        let after = std::fs::read(&path).unwrap();
        if matches!(case, 1 | 2) {
            assert_eq!(after, before);
        } else {
            let value: Value = aterm_json::from_slice(&after).unwrap();
            assert_eq!(value["refresh_token"].as_str(), Some("rotated-refresh"));
        }
        drop(gate);
        if let Some(pause) = pause.into_inner() {
            assert!(pause.try_wait().is_ok());
        }
    }
}

#[test]
fn coordinated_renewal_rereads_peers_and_fences_response_publication() {
    for case in 0..6 {
        let dir = stage(Region::Global);
        let path = credential_path(dir.path(), Region::Global);
        let before = std::fs::read(&path).unwrap();
        let lock_path = dir.path().join("oauth").join(format!(
            "{}.lock",
            path.file_stem().unwrap().to_str().unwrap()
        ));
        let held = (case == 1).then(|| {
            native_lock::NativeLock::acquire(
                dir.path(),
                Region::Global,
                deadline(),
                Arc::new(aterm_http::AlwaysAuthorized),
            )
            .unwrap()
        });
        let authority = Arc::new(Gate(AtomicBool::new(true)));
        let reads = std::cell::Cell::new(0);
        let exchanges = std::cell::Cell::new(0);
        let result = coordinated_using(
            dir.path(),
            Target {
                region: Region::Global,
                expected: EMAIL,
                on_rejected: None,
            },
            1000,
            deadline(),
            bound(dir.path(), authority.clone()),
            |now, guard| {
                assert!(guard.is_authorized());
                reads.set(reads.get() + 1);
                if reads.get() == 1 {
                    if case == 0 {
                        // A native peer completes after the first observation.
                        drop(proof_fixture(dir.path(), Region::Global, EMAIL));
                    }
                    return Err(NativeError::Expired);
                }
                assert!(
                    lock_path.is_dir(),
                    "post-lock observation owns native exclusion"
                );
                if case == 0 || reads.get() == 3 {
                    return verify_with(dir.path(), Region::Global, EMAIL, now, |_, _| {
                        Ok((
                            200,
                            aterm_json::to_vec(&aterm_json::json!({
                                "email": EMAIL, "user_id": "canonical-full-user"
                            }))
                            .unwrap(),
                        ))
                    });
                }
                Err(NativeError::Expired)
            },
            |_, _, guard| {
                exchanges.set(exchanges.get() + 1);
                assert!(guard.is_authorized());
                assert!(lock_path.is_dir());
                if case == 3 {
                    std::fs::rename(&lock_path, lock_path.with_extension("retained")).unwrap();
                    std::fs::create_dir(&lock_path).unwrap();
                }
                if case == 4 {
                    authority.0.store(false, Ordering::SeqCst);
                }
                if case == 5 {
                    return Err(NativeError::Temporary);
                }
                Ok((200, aterm_secrets::SecretBytes::from(RENEWED)))
            },
        );
        if case == 0 || case == 2 {
            let proof = result.unwrap().unwrap();
            proof.check_current().unwrap();
            if case == 2 {
                assert!(
                    lock_path.is_dir(),
                    "verified refresh retains its lease through publication"
                );
                assert!(
                    native_lock::NativeLock::acquire(
                        dir.path(),
                        Region::Global,
                        deadline(),
                        Arc::new(aterm_http::AlwaysAuthorized)
                    )
                    .is_err()
                );
                std::fs::rename(&lock_path, lock_path.with_extension("retained")).unwrap();
                assert!(
                    proof.check_current().is_err(),
                    "a lost retained lease invalidates the proof"
                );
            }
            drop(proof);
        } else {
            assert!(matches!(result, Err(NativeError::Temporary)), "case {case}");
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        assert_eq!(exchanges.get(), i32::from(case >= 2));
        assert_eq!(
            reads.get(),
            if case == 1 {
                1
            } else if case == 2 {
                3
            } else {
                2
            }
        );
        assert_eq!(lock_path.exists(), case == 1 || case == 3);
        drop(held);
    }
}

#[test]
fn automatic_dispatch_renews_only_expired_or_canonically_rejected_grants() {
    for case in 0..10 {
        let dir = stage(Region::Global);
        let reads = std::cell::Cell::new(0);
        let exchanges = std::cell::Cell::new(0);
        let result = run_using(
            dir.path(),
            Target {
                region: Region::Global,
                expected: EMAIL,
                on_rejected: None,
            },
            1000,
            deadline(),
            &aterm_http::AlwaysAuthorized,
            |now| {
                reads.set(reads.get() + 1);
                if reads.get() == 1 {
                    return match case {
                        0 => Ok(None),
                        1 | 7 | 9 => Err(NativeError::Expired),
                        2 | 8 => Err(NativeError::Rejected),
                        3 => Err(NativeError::Temporary),
                        4 => Err(NativeError::Local("unsupported local seed".into())),
                        5 => Err(NativeError::Refused("canonical refusal".into())),
                        _ => Ok(Some(proof_fixture(dir.path(), Region::Global, EMAIL))),
                    };
                }
                if case == 7 {
                    return Err(NativeError::Temporary);
                }
                if case == 8 {
                    return Err(NativeError::Rejected);
                }
                if case == 9 {
                    return Err(NativeError::Expired);
                }
                verify_with(dir.path(), Region::Global, EMAIL, now, |_, access| {
                    assert_eq!(access, "rotated-access");
                    Ok((
                        200,
                        aterm_json::to_vec(
                            &aterm_json::json!({"email":EMAIL,"user_id":"full-user"}),
                        )
                        .unwrap(),
                    ))
                })
            },
            |_, _| {
                exchanges.set(exchanges.get() + 1);
                Ok((200, aterm_secrets::SecretBytes::from(RENEWED)))
            },
        );
        assert_eq!(
            exchanges.get(),
            i32::from(matches!(case, 1 | 2 | 7 | 8 | 9))
        );
        assert_eq!(
            reads.get(),
            if matches!(case, 1 | 2 | 7 | 8 | 9) {
                2
            } else {
                1
            }
        );
        if matches!(case, 8 | 9) {
            assert!(matches!(&result, Err(NativeError::Temporary)));
        }
        if matches!(case, 1 | 2 | 6) {
            result.unwrap().unwrap().check_current().unwrap();
        } else if case == 0 {
            assert!(result.unwrap().is_none());
        } else {
            assert!(result.is_err());
        }
        if matches!(case, 7..=9) {
            let value: Value = aterm_json::from_slice(
                &std::fs::read(credential_path(dir.path(), Region::Global)).unwrap(),
            )
            .unwrap();
            assert_eq!(value["refresh_token"].as_str(), Some("rotated-refresh"));
        }
    }
}

#[test]
fn canonical_gateway_or_malformed_response_does_not_dispatch_refresh() {
    for (status, reply) in [
        (403, b"<html>security gateway</html>".as_slice()),
        (400, b"{}"),
        (429, b"{}"),
        (503, b"{}"),
        (200, b"not-json"),
        (200, br#"{"email":"person@example.com"}"#),
        (200, br#"{"email":"other@example.com","user_id":"full-id"}"#),
    ] {
        let dir = stage(Region::Global);
        drop(proof_fixture(dir.path(), Region::Global, EMAIL));
        let path = credential_path(dir.path(), Region::Global);
        let before = std::fs::read(&path).unwrap();
        let result = run_using(
            dir.path(),
            Target {
                region: Region::Global,
                expected: EMAIL,
                on_rejected: None,
            },
            1000,
            deadline(),
            &aterm_http::AlwaysAuthorized,
            |now| {
                verify_with(dir.path(), Region::Global, EMAIL, now, |_, _| {
                    Ok((status, reply.to_vec()))
                })
            },
            |_, _| panic!("an identity outage or mismatch must not exchange a refresh grant"),
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[test]
fn refresh_is_pinned_encoded_and_persisted_before_canonical_verification() {
    for region in [Region::Global, Region::MainlandCn] {
        let dir = stage(region);
        let path = credential_path(dir.path(), region);
        let seed = br#"{"access_token":"seed","refresh_token":"a+/&=~","expires_at":0,"token_type":"Bearer"}"#;
        aterm_secrets::fsx::write_atomic(&path, seed, 0o600).unwrap();
        renew_using(dir.path(), Target { region, expected: EMAIL, on_rejected: None }, 1000, deadline(), &aterm_http::AlwaysAuthorized, |url, body| {
            assert_eq!(url, format!("{}/api/oauth/token", issuer(region)));
            assert_eq!(std::str::from_utf8(body).unwrap(), format!("client_id={CLIENT_ID}&grant_type=refresh_token&refresh_token=a%2B%2F%26%3D~"));
            Ok((200, aterm_secrets::SecretBytes::from(RENEWED)))
        }).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let value: Value = aterm_json::from_slice(&bytes).unwrap();
        assert_eq!(value["access_token"].as_str(), Some("rotated-access"));
        assert_eq!(value["refresh_token"].as_str(), Some("rotated-refresh"));
        assert!(value["expires_at"].as_i64().unwrap() >= 4600);
        assert_eq!(value["token_type"].as_str(), Some("Bearer"));
        assert!(value.get("diagnostic").is_none());
        assert!(
            verify_with(dir.path(), region, EMAIL, 1000, |_, _| Err(
                "canonical outage".into()
            ))
            .is_err()
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "an identity outage must retain the rotated grant"
        );
        verify_with(dir.path(), region, EMAIL, 1000, |_, bearer| {
            assert_eq!(bearer, "rotated-access");
            Ok((
                200,
                aterm_json::to_vec(&aterm_json::json!({"email":EMAIL,"user_id":"full-user"}))
                    .unwrap(),
            ))
        })
        .unwrap()
        .unwrap()
        .check_current()
        .unwrap();
    }
}

#[test]
fn only_confirmed_refresh_rejection_persists_native_revocation() {
    for (status, body, rejected) in [
        (401, b"not-json".as_slice(), false),
        (403, b"{}", false),
        (401, br#"{"error":"invalid_grant"}"#, true),
        (403, br#"{"error":"invalid_grant"}"#, true),
        (400, br#"{"error":"invalid_grant"}"#, true),
        (429, b"{}", false),
        (503, b"{}", false),
        (400, br#"{"error":"invalid_request"}"#, false),
        (200, br#"{"access_token":"new","expires_in":60}"#, false),
        (
            200,
            br#"{"access_token":"new","refresh_token":"new","expires_in":0}"#,
            false,
        ),
        (
            200,
            br#"{"access_token":"new","refresh_token":"new","expires_in":60,"token_type":"Other"}"#,
            false,
        ),
    ] {
        let dir = stage(Region::Global);
        let path = credential_path(dir.path(), Region::Global);
        let before = std::fs::read(&path).unwrap();
        let error = renew_using(
            dir.path(),
            Target {
                region: Region::Global,
                expected: EMAIL,
                on_rejected: None,
            },
            1000,
            deadline(),
            &aterm_http::AlwaysAuthorized,
            |_, _| Ok((status, aterm_secrets::SecretBytes::from(body))),
        )
        .unwrap_err();
        assert_eq!(matches!(error, NativeError::RefreshRejected), rejected);
        if rejected {
            let value: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(value["access_token"].as_str(), Some(""));
            assert_eq!(value["refresh_token"].as_str(), Some(""));
            assert_eq!(value["expires_at"].as_i64(), Some(0));
            assert!(
                verify_or_refresh(
                    dir.path(),
                    Region::Global,
                    EMAIL,
                    1000,
                    deadline(),
                    Arc::new(aterm_http::AlwaysAuthorized)
                )
                .is_err()
            );
        } else {
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
}

#[test]
fn renewal_preserves_newer_files_and_respects_revocation_and_deadline() {
    for case in 0..5 {
        let dir = stage(Region::Global);
        let path = credential_path(dir.path(), Region::Global);
        let before = std::fs::read(&path).unwrap();
        let gate = Gate(AtomicBool::new(case != 3));
        let end = if case == 4 {
            login_drive::Deadline::after(std::time::Duration::ZERO)
        } else {
            deadline()
        };
        assert!(
            renew_using(
                dir.path(),
                Target {
                    region: Region::Global,
                    expected: EMAIL,
                    on_rejected: None
                },
                1000,
                end,
                &gate,
                |_, _| {
                    assert!(case < 3, "inert refusal must not exchange a grant");
                    if case == 0 {
                        gate.0.store(false, Ordering::SeqCst);
                    }
                    if case == 1 {
                        aterm_secrets::fsx::write_atomic(&path, b"newer-writer", 0o600).unwrap();
                    }
                    if case == 2 {
                        return Err(NativeError::Temporary);
                    }
                    Ok((200, aterm_secrets::SecretBytes::from(RENEWED)))
                }
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(&path).unwrap().as_slice(),
            if case == 1 {
                b"newer-writer".as_slice()
            } else {
                before.as_slice()
            }
        );
    }
}

#[test]
fn confirmed_rejection_survives_restart_without_reusing_the_grant_or_a_new_login() {
    for before_native_write in [false, true] {
        let dir = stage(Region::Global);
        let path = credential_path(dir.path(), Region::Global);
        let before = std::fs::read(&path).unwrap();
        let target = Target {
            region: Region::Global,
            expected: EMAIL,
            on_rejected: None,
        };
        if before_native_write {
            let config = FileEvidence::read(dir.path().join("config.toml"), false)
                .unwrap()
                .unwrap();
            let credential = FileEvidence::read(path.clone(), true).unwrap().unwrap();
            rejection::save(dir.path(), target, &config, &credential).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), before);
        } else {
            let error = renew_using(
                dir.path(),
                target,
                1000,
                deadline(),
                &aterm_http::AlwaysAuthorized,
                |_, _| {
                    Ok((
                        400,
                        aterm_secrets::SecretBytes::from(
                            br#"{"error":"invalid_grant"}"#.as_slice(),
                        ),
                    ))
                },
            )
            .unwrap_err();
            assert!(matches!(error, NativeError::RefreshRejected));
            assert_eq!(std::fs::read(&path).unwrap(), REVOKED);
        }
        let marker = std::fs::read(rejection::path(dir.path(), Region::Global)).unwrap();
        let text = String::from_utf8(marker.clone()).unwrap();
        assert!(!text.contains("fixture-access") && !text.contains("fixture-refresh"));
        let current = std::fs::read(&path).unwrap();
        let result = coordinated_using(
            dir.path(),
            target,
            1000,
            deadline(),
            bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized)),
            |now, _| {
                verify_with(dir.path(), Region::Global, EMAIL, now, |_, _| {
                    panic!("rejected snapshot reached canonical HTTP")
                })
            },
            |_, _, _| panic!("rejected snapshot repeated a refresh exchange"),
        );
        assert!(matches!(result, Err(NativeError::PreviouslyRejected)));
        assert_eq!(std::fs::read(&path).unwrap(), current);
        assert_eq!(
            std::fs::read(rejection::path(dir.path(), Region::Global)).unwrap(),
            marker
        );
    }
}

#[test]
fn rejected_snapshot_cannot_reclassify_another_identity_config_or_credential() {
    for case in 0..6 {
        let dir = stage(Region::Global);
        let target = Target {
            region: Region::Global,
            expected: EMAIL,
            on_rejected: None,
        };
        let config = FileEvidence::read(dir.path().join("config.toml"), false)
            .unwrap()
            .unwrap();
        let credential = FileEvidence::read(credential_path(dir.path(), Region::Global), true)
            .unwrap()
            .unwrap();
        rejection::save(dir.path(), target, &config, &credential).unwrap();
        let expected = if case == 1 {
            "another@example.com"
        } else {
            EMAIL
        };
        if case == 2 {
            let path = credential_path(dir.path(), Region::Global);
            aterm_secrets::fsx::write_atomic(&path, RENEWED, 0o600).unwrap();
        }
        if case == 3 {
            let mut bytes = std::fs::read(dir.path().join("config.toml")).unwrap();
            bytes.extend_from_slice(b"\n# a new config snapshot\n");
            aterm_secrets::fsx::write_atomic(&dir.path().join("config.toml"), &bytes, 0o600)
                .unwrap();
        }
        if case == 4 {
            aterm_secrets::fsx::write_atomic(
                &rejection::path(dir.path(), Region::Global),
                b"bad JSON",
                0o600,
            )
            .unwrap();
        }
        if case == 5 {
            let parent = dir.path().join("oauth");
            std::fs::rename(&parent, dir.path().join("retained-oauth")).unwrap();
            std::os::unix::fs::symlink(dir.path().join("retained-oauth"), &parent).unwrap();
        }
        let matched = rejection::matches(
            dir.path(),
            Target {
                region: Region::Global,
                expected,
                on_rejected: None,
            },
        );
        if case == 5 {
            assert!(matched.is_err());
        } else {
            assert_eq!(matched.unwrap(), case == 0, "case {case}");
        }
        if case != 5 {
            assert!(
                !rejection::matches(
                    dir.path(),
                    Target {
                        region: Region::MainlandCn,
                        expected: EMAIL,
                        on_rejected: None
                    }
                )
                .unwrap()
            );
        }
    }
}

#[test]
fn durable_rejection_observation_precedes_native_write_and_retries_only_the_record() {
    let dir = stage(Region::Global);
    let path = credential_path(dir.path(), Region::Global);
    let before = std::fs::read(&path).unwrap();
    let attempts = std::cell::Cell::new(0);
    let unavailable = || {
        attempts.set(attempts.get() + 1);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "native state changed before durable rejection"
        );
        Err("recovery storage unavailable".to_owned())
    };
    let result = renew_using(
        dir.path(),
        Target {
            region: Region::Global,
            expected: EMAIL,
            on_rejected: Some(&unavailable),
        },
        1000,
        deadline(),
        &aterm_http::AlwaysAuthorized,
        |_, _| {
            Ok((
                400,
                aterm_secrets::SecretBytes::from(br#"{"error":"invalid_grant"}"#.as_slice()),
            ))
        },
    );
    assert!(matches!(result, Err(NativeError::Temporary)));
    assert_eq!(attempts.get(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let recorded = std::cell::Cell::new(false);
    let available = || {
        recorded.set(true);
        Ok(())
    };
    let result = coordinated_using(
        dir.path(),
        Target {
            region: Region::Global,
            expected: EMAIL,
            on_rejected: Some(&available),
        },
        1000,
        deadline(),
        bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized)),
        |now, _| {
            verify_with(dir.path(), Region::Global, EMAIL, now, |_, _| {
                panic!("expired seed reached canonical HTTP")
            })
        },
        |_, _, _| panic!("recording retry repeated the rejected exchange"),
    );
    assert!(matches!(result, Err(NativeError::PreviouslyRejected)));
    assert!(recorded.get());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn rejected_snapshot_binds_recovery_to_exact_source_files_and_parent() {
    for region in [Region::Global, Region::MainlandCn] {
        let dir = stage(region);
        rejection_fixture(dir.path(), region, EMAIL);
        let snapshot = rejected_snapshot(dir.path(), region, EMAIL)
            .unwrap()
            .unwrap();
        snapshot.check_current().unwrap();
        let parent = login_drive::RecoveryKey([1; 32]);
        let key = snapshot.recovery_key(parent);
        assert_eq!(
            key,
            rejected_snapshot(dir.path(), region, EMAIL)
                .unwrap()
                .unwrap()
                .recovery_key(parent)
        );
        assert_ne!(
            key,
            snapshot.recovery_key(login_drive::RecoveryKey([2; 32]))
        );
        for file in [
            dir.path().join("config.toml"),
            credential_path(dir.path(), region),
            rejection::path(dir.path(), region),
        ] {
            let original = std::fs::read(&file).unwrap();
            let mut changed = original.clone();
            changed.push(b'\n');
            aterm_secrets::fsx::write_atomic(&file, &changed, 0o600).unwrap();
            assert!(snapshot.check_current().is_err());
            if let Some(changed_snapshot) = rejected_snapshot(dir.path(), region, EMAIL).unwrap() {
                assert_ne!(key, changed_snapshot.recovery_key(parent));
            }
            aterm_secrets::fsx::write_atomic(&file, &original, 0o600).unwrap();
            snapshot.check_current().unwrap();
        }
    }
}

/// A connector that refuses every connect and keeps what each request's deadline left it.
#[derive(Debug, Default)]
struct Refusing(std::sync::Mutex<Vec<Option<std::time::Duration>>>);
impl aterm_http::Connect for Refusing {
    fn connect(
        &self,
        _host: &str,
        _port: u16,
        deadline: aterm_http::Deadline,
    ) -> std::io::Result<std::net::TcpStream> {
        self.0.lock().unwrap().push(deadline.remaining());
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "a test sends nothing",
        ))
    }
}

/// A REFRESH HANDED A SIGN-IN'S WHOLE BUDGET WAITS ITS OWN CAP (KI-1105-14): a resumed Google
/// federation recovery hands the refresh what is left of its sign-in's 15 minutes, and the token
/// request is given [`REFRESH_REQUEST`] of it at most, so one in flight while the host sleeps ends
/// within that of the host waking. The expired seed reads no account first; the one request is
/// refused at its connect.
#[test]
fn a_refresh_handed_a_sign_ins_whole_budget_waits_its_own_cap() {
    let dir = stage(Region::Global);
    let refusing = Arc::new(Refusing::default());
    let result = verify_target(
        dir.path(),
        Target {
            region: Region::Global,
            expected: EMAIL,
            on_rejected: None,
        },
        1000,
        login_drive::Deadline::after(login_drive::HANG),
        bound(dir.path(), Arc::new(aterm_http::AlwaysAuthorized)),
        refusing.clone(),
    );
    assert!(result.is_err(), "a refused request renews nothing");
    let seen = refusing.0.lock().unwrap();
    let [left] = seen.as_slice() else {
        panic!("one refresh request: {seen:?}");
    };
    assert!(
        left.is_none_or(|left| left <= REFRESH_REQUEST),
        "the refresh waits its own cap, not the 15 minutes its caller left: {left:?}"
    );
}
