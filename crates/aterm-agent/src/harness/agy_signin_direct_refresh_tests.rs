// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::tests::{proof, setup};
use super::*;
use crate::harness::{browser_window::ProfileLock, google_profile};
use std::cell::Cell;

const OLD: &[u8] = br#"{"auth_method":"consumer","token":{"access_token":"old-access","refresh_token":"old-refresh","expiry":"2100-01-01T00:00:00Z"}}"#;

fn installed(state: &Path, home: &Path, program: &Path, deadline: WakeDeadline) -> PathBuf {
    let grant = super::super::direct_with(
        program,
        state,
        home,
        "one",
        deadline,
        super::super::direct_tests::fresh,
    )
    .unwrap();
    let path = grant.path().to_owned();
    drop(grant);
    aterm_secrets::fsx::write_atomic(&path, OLD, 0o600).unwrap();
    path
}

#[test]
fn direct_refresh_publishes_only_a_verified_replacement_under_the_original_claim() {
    for case in 0..11 {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, identity) = setup(root.path());
        let program = std::env::current_exe().unwrap();
        let deadline = WakeDeadline::after(Duration::from_secs(60));
        let path = installed(&state, &home, &program, deadline);
        let profile = native_profile(&state, "one", &identity);
        let record = super::super::direct_tests::direct_file(&state);
        let before = std::fs::read(&record).unwrap();
        let called = Cell::new(false);
        let _held = (case == 9).then(|| ProfileLock::claim(&profile).unwrap());
        let request_deadline = if case == 10 {
            WakeDeadline::after(Duration::ZERO)
        } else {
            deadline
        };
        let result = run_with(
            &program,
            &state,
            &home,
            "one",
            request_deadline,
            |token, expected, wait| {
                assert!(ProfileLock::claim(&profile).is_err());
                if token == "old-access" {
                    match case {
                        1 => return Err(google_profile::Error::Unavailable),
                        2 => return Err(google_profile::Error::RateLimited),
                        3 => aterm_secrets::fsx::write_atomic(&path, b"changed", 0o600).unwrap(),
                        4 => std::fs::write(state.join("google-identities.toml"), b"changed")
                            .unwrap(),
                        _ => {}
                    }
                    return Err(google_profile::Error::CredentialRejected);
                }
                assert_eq!(token, "synthetic-token");
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    if case == 5 {
                        b"changed".as_slice()
                    } else {
                        OLD
                    }
                );
                if case == 6 {
                    return Err(google_profile::Error::Refused("wrong account"));
                }
                proof(token, expected, wait)
            },
            |program, state, home, account, staging, seen_deadline| {
                assert_eq!(std::fs::read(&path).unwrap(), OLD);
                assert_eq!(seen_deadline, deadline);
                assert!(ProfileLock::claim(&profile).is_err());
                called.set(true);
                let staged = staging.as_ref().unwrap();
                assert_ne!(staged.profile, profile);
                let seed = staged
                    .previous
                    .unwrap()
                    .join(".gemini/antigravity-cli/antigravity-oauth-token");
                let seed: aterm_json::Value =
                    aterm_json::from_slice(&std::fs::read(seed).unwrap()).unwrap();
                assert_eq!(seed["token"]["refresh_token"].as_str(), Some("old-refresh"));
                assert_eq!(
                    seed["token"]["expiry"].as_str(),
                    Some("2000-01-01T00:00:00Z")
                );
                if case == 7 {
                    return Err(GoogleLoginError::Native(Error::Native));
                }
                let grant = super::super::tests::fresh(
                    program,
                    state,
                    home,
                    account,
                    staging,
                    seen_deadline,
                )?;
                if case == 5 {
                    aterm_secrets::fsx::write_atomic(&path, b"changed", 0o600).unwrap();
                }
                if case == 8 {
                    aterm_secrets::fsx::write_atomic(&record, b"changed", 0o600).unwrap();
                }
                Ok(grant)
            },
        );
        assert_eq!(called.get(), case == 0 || (5..=8).contains(&case));
        assert_eq!(result.is_ok(), case == 0, "case {case}");
        if let Ok(grant) = result {
            assert_eq!(grant.path(), path);
            let verified = grant
                .verify_with_retry(|| deadline.remaining(), proof, |_| {})
                .unwrap();
            verified.finish_recovery().unwrap();
            assert!(!record.exists());
        } else {
            assert_eq!(
                std::fs::read(&path).unwrap(),
                if case == 3 || case == 5 {
                    b"changed".as_slice()
                } else {
                    OLD
                }
            );
            assert_eq!(
                std::fs::read(&record).unwrap(),
                if case == 8 {
                    b"changed".as_slice()
                } else {
                    before.as_slice()
                }
            );
        }
    }
}

#[test]
fn direct_refresh_resumes_a_completed_native_effect_after_verification_outage_or_rate_limit() {
    for unavailable in [
        google_profile::Error::Unavailable,
        google_profile::Error::RateLimited,
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, _) = setup(root.path());
        let program = std::env::current_exe().unwrap();
        let deadline = WakeDeadline::after(Duration::from_secs(60));
        let path = installed(&state, &home, &program, deadline);
        let result = run_with(
            &program,
            &state,
            &home,
            "one",
            deadline,
            |token, _, _| {
                Err(if token == "old-access" {
                    google_profile::Error::CredentialRejected
                } else {
                    unavailable
                })
            },
            super::super::tests::fresh,
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), OLD);
        let grant = run_with(
            &program,
            &state,
            &home,
            "one",
            deadline,
            |token, expected, wait| {
                if token == "old-access" {
                    Err(google_profile::Error::CredentialRejected)
                } else {
                    proof(token, expected, wait)
                }
            },
            |_, _, _, _, _, _| panic!("a completed native refresh was repeated"),
        )
        .unwrap();
        assert_eq!(grant.path(), path);
        assert_ne!(std::fs::read(&path).unwrap(), OLD);
        grant
            .verify_with_retry(|| deadline.remaining(), proof, |_| {})
            .unwrap()
            .finish_recovery()
            .unwrap();
    }
}

#[test]
fn direct_refresh_crash_stand_in() {
    let Some(root) = std::env::var_os("ATERM_TEST_DIRECT_REFRESH_ROOT").map(PathBuf::from) else {
        return;
    };
    let _result = run_with(
        &std::env::current_exe().unwrap(),
        &root.join("state"),
        &root.join("home"),
        "one",
        WakeDeadline::after(Duration::from_secs(60)),
        |token, _, _| {
            if token == "old-access" {
                return Err(google_profile::Error::CredentialRejected);
            }
            std::fs::write(root.join("renewal-completed"), b"ready").unwrap();
            aterm_uds::process::kill_self_now();
        },
        super::super::tests::fresh,
    );
    panic!("refresh did not reach the verification checkpoint");
}

#[test]
fn direct_refresh_survives_process_death_before_live_publication() {
    use std::os::unix::process::ExitStatusExt;
    let root = aterm_tempfile::tempdir().unwrap();
    let (state, home, _) = setup(root.path());
    let program = std::env::current_exe().unwrap();
    let deadline = WakeDeadline::after(Duration::from_secs(60));
    let path = installed(&state, &home, &program, deadline);
    let mut child = aterm_uds::spawnfd::command(&program);
    child
        .args([
            "harness::agy_signin::recovery::direct_refresh::tests::direct_refresh_crash_stand_in",
            "--exact",
            "--nocapture",
        ])
        .env("ATERM_TEST_DIRECT_REFRESH_ROOT", root.path());
    let output = aterm_uds::deadline::output_within(&mut child, Duration::from_secs(60)).unwrap();
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.path().join("renewal-completed").is_file());
    assert_eq!(std::fs::read(&path).unwrap(), OLD);
    let grant = run_with(
        &program,
        &state,
        &home,
        "one",
        deadline,
        |token, expected, wait| {
            if token == "old-access" {
                Err(google_profile::Error::CredentialRejected)
            } else {
                proof(token, expected, wait)
            }
        },
        |_, _, _, _, _, _| panic!("restart repeated a completed native refresh"),
    )
    .unwrap();
    assert_eq!(grant.path(), path);
    assert_ne!(std::fs::read(&path).unwrap(), OLD);
    grant
        .verify_with_retry(|| deadline.remaining(), proof, |_| {})
        .unwrap()
        .finish_recovery()
        .unwrap();
}

#[test]
#[ignore = "contacts managed agy anonymously; ATERM_TEST_AGY names its pinned binary"]
fn rejected_direct_refresh_reaches_native_auth_without_changing_live_credentials() {
    let program = std::env::var_os("ATERM_TEST_AGY")
        .map(PathBuf::from)
        .unwrap();
    let root = aterm_tempfile::tempdir().unwrap();
    let (state, home, _) = setup(root.path());
    let deadline = WakeDeadline::after(Duration::from_secs(60));
    let path = installed(&state, &home, &program, deadline);
    let observed = Cell::new(false);
    let result = run_with(
        &program,
        &state,
        &home,
        "one",
        deadline,
        |_, _, _| Err(google_profile::Error::CredentialRejected),
        |program, _, _, _, staging, deadline| {
            assert_eq!(std::fs::read(&path).unwrap(), OLD);
            let staging = staging.unwrap();
            let claim = stage_profile(staging.profile, staging.previous)
                .map_err(GoogleLoginError::Native)?;
            run_claimed(
                program,
                staging.profile,
                Some(claim),
                None,
                deadline,
                |_, _| {
                    observed.set(true);
                    Err(Error::Callback)
                },
            )
            .map_err(GoogleLoginError::Native)?;
            Err(GoogleLoginError::Native(Error::Callback))
        },
    );
    assert!(observed.get());
    assert!(matches!(
        result,
        Err(GoogleLoginError::Native(Error::Callback))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), OLD);
}
