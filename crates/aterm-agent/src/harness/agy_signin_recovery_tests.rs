// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::{google_browser, google_browser_store, google_identity, google_profile};
use std::cell::Cell;

const REGISTRY: &[u8] = b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
const COOKIES: &[u8] = br#"[{"name":"fixture","value":"pending-cookie-secret","domain":".google.com","path":"/","secure":true}]"#;

pub(super) fn setup(root: &Path) -> (PathBuf, PathBuf, google_identity::Identity) {
    let state = root.join("state");
    let home = root.join("home");
    aterm_secrets::fsx::ensure_private_dir(&state).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
    aterm_secrets::fsx::write_atomic(&state.join("google-identities.toml"), REGISTRY, 0o600)
        .unwrap();
    let identity = registry(&state)
        .unwrap()
        .resolve("agy", "one")
        .unwrap()
        .0
        .clone();
    (state, home, identity)
}

pub(super) fn fresh(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staging: Option<StagedGoogleLogin<'_>>,
    _: Instant,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    let staging = staging.unwrap();
    let mut grant =
        super::super::publication_fixture(state, staging.profile, account, staging.expected_email)
            .grant;
    grant.native.program = program.into();
    let identity = registry(state)
        .unwrap()
        .resolve("agy", account)
        .unwrap()
        .0
        .clone();
    grant.native.session = Some(google_browser::pending_fixture(
        &identity, state, home, COOKIES,
    ));
    Ok(grant)
}

pub(super) fn staging(profile: &Path, key: u8) -> StagedGoogleLogin<'_> {
    StagedGoogleLogin {
        profile,
        previous: None,
        expected_email: "person@example.com",
        recovery: Some(RecoveryKey([key; 32])),
    }
}

pub(super) fn proof(
    _: &str,
    expected: &str,
    _: Duration,
) -> Result<google_profile::GoogleProfile, google_profile::Error> {
    Ok(google_profile::GoogleProfile {
        email: expected.into(),
        principal: [1; 32],
    })
}

pub(super) fn candidate_file(state: &Path) -> PathBuf {
    let entries = std::fs::read_dir(state.join("pending-native-google"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    entries[0].path().join("candidate.age")
}

#[test]
fn pending_crash_stand_in() {
    let Some(root) = std::env::var_os("ATERM_TEST_PENDING_CRASH_ROOT").map(PathBuf::from) else {
        return;
    };
    let state = root.join("state");
    let home = root.join("home");
    let exe = std::env::current_exe().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let grant = run_with(
        &exe,
        &state,
        &home,
        "one",
        staging(&root.join("first"), 4),
        deadline,
        fresh,
    )
    .unwrap();
    let _retained: Result<VerifiedGoogleGrant, UnverifiedGoogleGrant> =
        if std::env::var_os("ATERM_TEST_PENDING_AFTER_PROOF").is_some() {
            Ok(grant
                .verify_with_retry(deadline, proof, |_| {}, Instant::now)
                .unwrap())
        } else {
            Err(grant)
        };
    std::fs::write(root.join("checkpoint-ready"), b"ready").unwrap();
    aterm_uds::process::kill_self_now();
}

#[test]
fn pending_native_completion_survives_process_death_and_removed_staging() {
    use std::os::unix::process::ExitStatusExt;
    for after_proof in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, identity) = setup(root.path());
        let claim = identity.claim(&state).unwrap();
        google_browser_store::save(&home, &identity, &claim, b"old-verified-snapshot").unwrap();
        let browser_snapshot = claim.profile().join("session.age");
        drop(claim);
        let exe = std::env::current_exe().unwrap();
        let mut child = aterm_uds::spawnfd::command(&exe);
        child
            .args([
                "harness::agy_signin::recovery::tests::pending_crash_stand_in",
                "--exact",
                "--nocapture",
            ])
            .env("ATERM_TEST_PENDING_CRASH_ROOT", root.path());
        if after_proof {
            child.env("ATERM_TEST_PENDING_AFTER_PROOF", "1");
        } else {
            child.env_remove("ATERM_TEST_PENDING_AFTER_PROOF");
        }
        let output =
            aterm_uds::deadline::output_within(&mut child, Duration::from_secs(60)).unwrap();
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGKILL),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(root.path().join("checkpoint-ready").is_file());
        let checkpoint = candidate_file(&state);
        let ciphertext = std::fs::read(&checkpoint).unwrap();
        for secret in [b"synthetic-token".as_slice(), b"pending-cookie-secret"] {
            assert!(!ciphertext.windows(secret.len()).any(|part| part == secret));
        }
        std::fs::remove_dir_all(root.path().join("first")).unwrap();
        let before = std::fs::read(&browser_snapshot).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let resumed = run_with(
            &exe,
            &state,
            &home,
            "one",
            staging(&root.path().join("second"), 4),
            deadline,
            |_, _, _, _, _, _| panic!("recovery started another native login"),
        )
        .unwrap();
        let calls = Cell::new(0);
        let verified = resumed
            .verify_with_retry(
                deadline,
                |token, expected, wait| {
                    calls.set(calls.get() + 1);
                    assert_eq!(token, "synthetic-token");
                    assert_eq!(std::fs::read(&browser_snapshot).unwrap(), before);
                    assert_eq!(std::fs::read(&checkpoint).unwrap(), ciphertext);
                    if calls.get() == 1 {
                        return Err(google_profile::Error::Unavailable);
                    }
                    proof(token, expected, wait)
                },
                |_| {},
                Instant::now,
            )
            .unwrap();
        assert_eq!(calls.get(), 2);
        assert!(
            checkpoint.is_file(),
            "verification retired the candidate before publication"
        );
        let claim = identity.claim(&state).unwrap();
        assert_eq!(
            google_browser_store::load(&home, &identity, &claim)
                .unwrap()
                .unwrap()
                .as_slice(),
            COOKIES
        );
        drop(claim);
        verified.finish_recovery().unwrap();
        assert!(!checkpoint.exists());
        assert!(
            verified.path().is_ok(),
            "retirement invalidated the verified grant"
        );
    }
}

#[test]
fn pending_native_recovery_refuses_corruption_wrong_machine_and_concurrent_owners() {
    for case in 0..4 {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, _) = setup(root.path());
        let exe = std::env::current_exe().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let grant = run_with(
            &exe,
            &state,
            &home,
            "one",
            staging(&root.path().join("first"), 4),
            deadline,
            fresh,
        )
        .unwrap();
        let path = candidate_file(&state);
        let original = std::fs::read(&path).unwrap();
        if case != 2 {
            drop(grant);
        }
        if case == 0 {
            let mut damaged = original.clone();
            let last = damaged.last_mut().unwrap();
            *last ^= 1;
            aterm_secrets::fsx::write_atomic(&path, &damaged, 0o600).unwrap();
        }
        let another = root.path().join("another-home");
        aterm_secrets::fsx::ensure_private_dir(&another).unwrap();
        let before = std::fs::read(&path).unwrap();
        let result = run_with(
            &exe,
            &state,
            if case == 1 { &another } else { &home },
            "one",
            staging(&root.path().join("second"), 4),
            if case == 3 { Instant::now() } else { deadline },
            |_, _, _, _, _, _| panic!("an unavailable checkpoint triggered authentication"),
        );
        assert!(result.is_err(), "case {case}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            aterm_secrets::client::read_machine_identity(&aterm_secrets::client::Paths::for_home(
                &another
            ))
            .unwrap()
            .is_none()
        );
    }
}

#[test]
fn pending_native_rejection_and_deadlines_preserve_refresh_recovery() {
    for case in 0..3 {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, _) = setup(root.path());
        let exe = std::env::current_exe().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let grant = run_with(
            &exe,
            &state,
            &home,
            "one",
            staging(&root.path().join("first"), 4),
            deadline,
            fresh,
        )
        .unwrap();
        let path = candidate_file(&state);
        let ciphertext = std::fs::read(&path).unwrap();
        let result = grant.verify_with_retry(
            if case == 2 { Instant::now() } else { deadline },
            |_, _, _| {
                assert_ne!(case, 2);
                Err(if case == 0 {
                    google_profile::Error::CredentialRejected
                } else {
                    google_profile::Error::Refused("wrong account")
                })
            },
            |_| {},
            Instant::now,
        );
        assert!(result.is_err());
        if case != 1 {
            assert_eq!(std::fs::read(&path).unwrap(), ciphertext);
        } else {
            assert!(!path.exists());
        }
    }
}

#[test]
fn rate_limits_preserve_direct_and_pool_completions_without_retry_or_new_login() {
    for direct in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, identity) = setup(root.path());
        let program = std::env::current_exe().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let first = root.path().join("first");
        let grant = if direct {
            super::direct_with(
                &program,
                &state,
                &home,
                "one",
                deadline,
                super::direct_tests::fresh,
            )
            .unwrap()
        } else {
            run_with(
                &program,
                &state,
                &home,
                "one",
                staging(&first, 4),
                deadline,
                fresh,
            )
            .unwrap()
        };
        let native = grant.path().to_owned();
        let original = std::fs::read(&native).unwrap();
        let record = if direct {
            super::direct_tests::direct_file(&state)
        } else {
            candidate_file(&state)
        };
        let before = std::fs::read(&record).unwrap();
        let calls = Cell::new(0);
        let result = grant.verify_with_retry(
            deadline,
            |_, _, _| {
                calls.set(calls.get() + 1);
                Err(google_profile::Error::RateLimited)
            },
            |_| panic!("rate limiting scheduled an immediate retry"),
            Instant::now,
        );
        assert!(matches!(result, Err(google_profile::Error::RateLimited)));
        assert_eq!(calls.get(), 1);
        assert_eq!(std::fs::read(&record).unwrap(), before);
        assert_eq!(std::fs::read(&native).unwrap(), original);
        let claim = identity.claim(&state).unwrap();
        assert!(
            google_browser_store::load(&home, &identity, &claim)
                .unwrap()
                .is_none()
        );
        drop(claim);
        let grant = if direct {
            super::direct_with(
                &program,
                &state,
                &home,
                "one",
                deadline,
                |_, _, _, _, _, _| panic!("rate limiting repeated native login"),
            )
            .unwrap()
        } else {
            run_with(
                &program,
                &state,
                &home,
                "one",
                staging(&root.path().join("second"), 4),
                deadline,
                |_, _, _, _, _, _| panic!("rate limiting repeated native login"),
            )
            .unwrap()
        };
        grant
            .verify_with_retry(deadline, proof, |_| {}, Instant::now)
            .unwrap()
            .finish_recovery()
            .unwrap();
        assert!(!record.exists());
    }
}

#[test]
fn changed_pending_record_is_not_deleted_or_sent_to_google() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (state, home, _) = setup(root.path());
    let exe = std::env::current_exe().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let grant = run_with(
        &exe,
        &state,
        &home,
        "one",
        staging(&root.path().join("first"), 4),
        deadline,
        fresh,
    )
    .unwrap();
    let path = candidate_file(&state);
    aterm_secrets::fsx::write_atomic(&path, b"different-owned-data", 0o600).unwrap();
    assert!(
        grant
            .verify_with_retry(
                deadline,
                |_, _, _| panic!("changed attempt contacted Google"),
                |_| {},
                Instant::now
            )
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), b"different-owned-data");
}

#[test]
fn pending_native_scope_tracks_authentication_and_selected_parent_not_unrelated_activity() {
    use crate::harness::accounts;
    let row = accounts::Row {
        label: "one".into(),
        email: Some("person@example.com".into()),
        ..Default::default()
    };
    let mut logins = accounts::Logins::default();
    let mut state = accounts::State::default();
    accounts::note_renewed(
        &mut state,
        &mut logins,
        "one",
        None,
        "/private/first",
        None,
        1,
    );
    let key = |row: &accounts::Row, state: &accounts::State, logins: &accounts::Logins| {
        accounts::RenewalParent::capture(logins, "one")
            .recovery_scope(row, state, logins)
            .unwrap()
    };
    let original = key(&row, &state, &logins);
    let mut reported = logins.clone();
    accounts::note_sign_in_attempt(
        &mut reported,
        "one",
        accounts::AttemptOutcome::Failed,
        "canonical verification unavailable",
        2,
    );
    accounts::note_sign_in_task(&mut reported, "one", accounts::SignInAction::Login, 2);
    assert_eq!(
        key(&row, &state, &reported),
        original,
        "diagnostic recording invalidated the completion it reports"
    );

    for case in 0..7 {
        let mut row = row.clone();
        let mut state = state.clone();
        let mut logins = logins.clone();
        match case {
            0 => row.email = Some("another@example.com".into()),
            1 => row.held_by = Some("another-machine".into()),
            2 => row.was.push(Some("/another/profile".into())),
            3 => {
                accounts::note_renewed(
                    &mut state,
                    &mut logins,
                    "one",
                    None,
                    "/private/next",
                    None,
                    2,
                );
            }
            4 => state.accounts.get_mut("one").unwrap().auth = accounts::Auth::SignedOut,
            5 => state.accounts.get_mut("one").unwrap().auth_at = Some(2),
            6 => logins.labels.get_mut("one").unwrap().ended_at = Some(2),
            _ => unreachable!(),
        }
        assert_ne!(key(&row, &state, &logins), original, "case {case}");
    }
    accounts::note_renewed(
        &mut state,
        &mut logins,
        "another",
        None,
        "/private/another",
        None,
        2,
    );
    logins.sign_in_clock = 17;
    assert_eq!(key(&row, &state, &logins), original);
}

#[test]
fn pending_native_scope_survives_recording_the_first_failed_login() {
    use crate::harness::accounts;
    let row = accounts::Row {
        label: "one".into(),
        email: Some("person@example.com".into()),
        ..Default::default()
    };
    let state = accounts::State::default();
    let mut logins = accounts::Logins::default();
    let key = |logins: &accounts::Logins| {
        accounts::RenewalParent::capture(logins, "one")
            .recovery_scope(&row, &state, logins)
            .unwrap()
    };
    let before = key(&logins);
    accounts::note_sign_in_attempt(
        &mut logins,
        "one",
        accounts::AttemptOutcome::Failed,
        "canonical verification unavailable",
        1,
    );
    accounts::note_sign_in_task(&mut logins, "one", accounts::SignInAction::Login, 1);
    assert!(
        logins.get("one").is_some(),
        "diagnostics should exercise creation of a report-only binding"
    );
    assert_eq!(key(&logins), before);
    logins.labels.get_mut("one").unwrap().dir = Some("/private/new-login".into());
    assert_ne!(
        key(&logins),
        before,
        "a real binding change must still revoke the old scope"
    );
}

#[test]
fn stale_pending_scope_starts_only_a_new_authorized_attempt() {
    for change_binding in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, _) = setup(root.path());
        let exe = std::env::current_exe().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let grant = run_with(
            &exe,
            &state,
            &home,
            "one",
            staging(&root.path().join("first"), 4),
            deadline,
            fresh,
        )
        .unwrap();
        drop(grant);
        if change_binding {
            aterm_secrets::fsx::write_atomic(
                &state.join("google-identities.toml"),
                std::str::from_utf8(REGISTRY)
                    .unwrap()
                    .replace("id='one'", "id='replacement'")
                    .replace("identity='one'", "identity='replacement'")
                    .as_bytes(),
                0o600,
            )
            .unwrap();
        }
        let called = Cell::new(false);
        let grant = run_with(
            &exe,
            &state,
            &home,
            "one",
            staging(
                &root.path().join("second"),
                if change_binding { 4 } else { 5 },
            ),
            deadline,
            |program, state, home, account, staging, deadline| {
                called.set(true);
                fresh(program, state, home, account, staging, deadline)
            },
        )
        .unwrap();
        assert!(
            called.get(),
            "stale authorization restored a cached native grant"
        );
        let verified = grant
            .verify_with_retry(deadline, proof, |_| {}, Instant::now)
            .unwrap();
        verified.finish_recovery().unwrap();
    }
}

#[test]
fn resumed_rejected_completion_renews_only_after_canonical_rejection() {
    const TOKEN: &[u8] = br#"{"auth_method":"consumer","token":{"access_token":"saved-access","refresh_token":"saved-refresh","expiry":"2100-01-01T00:00:00Z"}}"#;
    for case in 0..8 {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, _) = setup(root.path());
        let exe = std::env::current_exe().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let first = root.path().join("first");
        let grant = run_with(
            &exe,
            &state,
            &home,
            "one",
            staging(&first, 4),
            deadline,
            |program, state, home, account, staging, deadline| {
                let grant = fresh(program, state, home, account, staging, deadline)?;
                aterm_secrets::fsx::write_atomic(
                    grant.path(),
                    if case == 7 {
                        br#"{"auth_method":"consumer","token":{"access_token":"saved-access"}}"#
                    } else {
                        TOKEN
                    },
                    0o600,
                )
                .unwrap();
                Ok(grant)
            },
        )
        .unwrap();
        drop(grant);
        let path = candidate_file(&state);
        let before = std::fs::read(&path).unwrap();
        let invoked = Cell::new(false);
        let second = root.path().join("second");
        let result = run_with_checked(
            &exe,
            &state,
            &home,
            "one",
            staging(&second, 4),
            deadline,
            |token, expected, wait| {
                assert_eq!(token, "saved-access");
                assert_eq!(expected, "person@example.com");
                assert!(wait <= Duration::from_secs(30));
                match case {
                    1 => return Err(google_profile::Error::Unavailable),
                    2 => return Err(google_profile::Error::RateLimited),
                    3 => std::fs::write(state.join("google-identities.toml"), b"changed").unwrap(),
                    4 => aterm_secrets::fsx::write_atomic(&path, b"changed", 0o600).unwrap(),
                    5 => return Ok(()),
                    _ => {}
                }
                Err(google_profile::Error::CredentialRejected)
            },
            |program, state, home, account, staging, seen_deadline| {
                assert!(
                    case == 0 || case == 6 || case == 7,
                    "non-401 or changed custody triggered native login"
                );
                invoked.set(true);
                assert_eq!(seen_deadline, deadline);
                assert_eq!(std::fs::read(&path).unwrap(), before);
                let staged = staging.as_ref().unwrap();
                if case == 7 {
                    assert!(staged.previous.is_none());
                } else {
                    let seed = staged
                        .previous
                        .unwrap()
                        .join(".gemini/antigravity-cli/antigravity-oauth-token");
                    let token: aterm_json::Value =
                        aterm_json::from_slice(&std::fs::read(seed).unwrap()).unwrap();
                    assert_eq!(
                        token["token"]["refresh_token"].as_str(),
                        Some("saved-refresh")
                    );
                    assert_eq!(
                        token["token"]["expiry"].as_str(),
                        Some("2000-01-01T00:00:00Z")
                    );
                }
                if case == 6 {
                    return Err(GoogleLoginError::Native(Error::Native));
                }
                fresh(program, state, home, account, staging, seen_deadline)
            },
        );
        assert_eq!(invoked.get(), case == 0 || case == 6 || case == 7);
        assert_eq!(result.is_ok(), case == 0 || case == 5 || case == 7);
        drop(result);
        if case != 7 {
            assert_eq!(
                std::fs::read(first.join(".gemini/antigravity-cli/antigravity-oauth-token"))
                    .unwrap(),
                TOKEN
            );
        }
        if case == 0 || case == 7 {
            assert_ne!(std::fs::read(&path).unwrap(), before);
        } else if case == 4 {
            assert_eq!(std::fs::read(&path).unwrap(), b"changed");
        } else {
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
}

#[test]
#[ignore = "contacts managed native agy anonymously; ATERM_TEST_AGY names its pinned binary"]
fn rejected_encrypted_completion_reaches_native_refresh_handoff() {
    let program = std::env::var_os("ATERM_TEST_AGY")
        .map(PathBuf::from)
        .unwrap();
    let root = aterm_tempfile::tempdir().unwrap();
    let (state, home, _) = setup(root.path());
    let deadline = Instant::now() + Duration::from_secs(60);
    let grant = run_with(&program, &state, &home, "one", staging(&root.path().join("first"), 4), deadline,
        |program, state, home, account, staging, deadline| {
            let grant = fresh(program, state, home, account, staging, deadline)?;
            aterm_secrets::fsx::write_atomic(grant.path(), br#"{"auth_method":"consumer","token":{"access_token":"aterm-synthetic-access","token_type":"Bearer","refresh_token":"aterm-synthetic-invalid-refresh","expiry":"2100-01-01T00:00:00Z"}}"#, 0o600).unwrap();
            Ok(grant)
        }).unwrap();
    drop(grant);
    let journal = candidate_file(&state);
    let original = std::fs::read(&journal).unwrap();
    let observed = Cell::new(false);
    let result = run_with_checked(
        &program,
        &state,
        &home,
        "one",
        staging(&root.path().join("second"), 4),
        deadline,
        |_, _, _| Err(google_profile::Error::CredentialRejected),
        |program, _, _, _, staging, deadline| {
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
    assert!(
        observed.get(),
        "native refresh never reached fresh authorization"
    );
    assert!(matches!(
        result,
        Err(GoogleLoginError::Native(Error::Callback))
    ));
    assert_eq!(std::fs::read(&journal).unwrap(), original);
}
