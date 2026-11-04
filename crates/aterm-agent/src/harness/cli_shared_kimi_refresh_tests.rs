// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

const EMAIL: &str = "person@example.com";
const RENEWED: &[u8] = br#"{"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600,"scope":"coding","diagnostic":"not-native-token-data"}"#;

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
        Arc::new(aterm_http::AlwaysAuthorized),
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
fn stage(region: Region) -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().unwrap();
    drop(proof_fixture(dir.path(), region, EMAIL));
    expire_pending(dir.path(), region).unwrap();
    dir
}
fn deadline() -> Instant {
    Instant::now() + std::time::Duration::from_secs(60)
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
            native_lock::NativeLock::acquire(dir.path(), Region::Global, deadline()).unwrap()
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
            authority.clone(),
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
                    native_lock::NativeLock::acquire(dir.path(), Region::Global, deadline())
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
            Instant::now()
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
            Arc::new(aterm_http::AlwaysAuthorized),
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
        Arc::new(aterm_http::AlwaysAuthorized),
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
