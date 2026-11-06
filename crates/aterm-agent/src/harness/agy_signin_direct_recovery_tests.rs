// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::tests::{candidate_file, proof, setup, staging};
use super::*;
use crate::harness::{google_browser, google_browser_store};
use std::cell::Cell;

const NEWER: &[u8] = br#"[{"name":"fixture","value":"newer-direct-session","domain":".google.com","path":"/","secure":true}]"#;

pub(super) fn fresh(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staged: Option<StagedGoogleLogin<'_>>,
    _: WakeDeadline,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    assert!(staged.is_none());
    let registry = registry(state).unwrap();
    let (identity, binding) = registry.resolve("agy", account).unwrap();
    let profile = native_profile(state, account, identity);
    let mut grant =
        super::super::publication_fixture(state, &profile, account, binding.expected_email()).grant;
    grant.native.program = program.into();
    grant.native.session = Some(google_browser::pending_fixture(
        identity, state, home, NEWER,
    ));
    Ok(grant)
}

pub(super) fn direct_file(state: &Path) -> PathBuf {
    let entries = std::fs::read_dir(state.join("pending-native-google-direct"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    entries[0].path().join("candidate.age")
}

#[test]
fn direct_crash_stand_in() {
    let Some(root) = std::env::var_os("ATERM_TEST_DIRECT_CRASH_ROOT").map(PathBuf::from) else {
        return;
    };
    let exe = std::env::current_exe().unwrap();
    let deadline = WakeDeadline::after(Duration::from_secs(60));
    let grant = direct_with(
        &exe,
        &root.join("state"),
        &root.join("home"),
        "one",
        deadline,
        fresh,
    )
    .unwrap();
    let _retained: Result<VerifiedGoogleGrant, UnverifiedGoogleGrant> =
        if std::env::var_os("ATERM_TEST_DIRECT_AFTER_PROOF").is_some() {
            Ok(grant
                .verify_with_retry(|| deadline.remaining(), proof, |_| {})
                .unwrap())
        } else {
            Err(grant)
        };
    std::fs::write(root.join("direct-ready"), b"ready").unwrap();
    aterm_uds::process::kill_self_now();
}

#[test]
fn direct_google_login_resumes_after_process_death_without_native_auth() {
    use std::os::unix::process::ExitStatusExt;
    for after_proof in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, identity) = setup(root.path());
        let exe = std::env::current_exe().unwrap();
        let mut child = aterm_uds::spawnfd::command(&exe);
        child
            .args([
                "harness::agy_signin::recovery::direct_tests::direct_crash_stand_in",
                "--exact",
                "--nocapture",
            ])
            .env("ATERM_TEST_DIRECT_CRASH_ROOT", root.path());
        if after_proof {
            child.env("ATERM_TEST_DIRECT_AFTER_PROOF", "1");
        } else {
            child.env_remove("ATERM_TEST_DIRECT_AFTER_PROOF");
        }
        let output =
            aterm_uds::deadline::output_within(&mut child, Duration::from_secs(60)).unwrap();
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGKILL),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(root.path().join("direct-ready").is_file());
        let profile = native_profile(&state, "one", &identity);
        let native = profile.join(".gemini/antigravity-cli/antigravity-oauth-token");
        let original = std::fs::read(&native).unwrap();
        let record = direct_file(&state);
        let ciphertext = std::fs::read(&record).unwrap();
        assert!(
            !ciphertext
                .windows(b"synthetic-token".len())
                .any(|bytes| bytes == b"synthetic-token")
        );
        let deadline = WakeDeadline::after(Duration::from_secs(60));
        // This test executable rejects agy's arguments. Success therefore proves the
        // recovery driver restored the pending result without executing native auth.
        let grant =
            super::direct_refresh::run_with(&exe, &state, &home, "one", deadline, proof, fresh)
                .unwrap();
        assert_eq!(std::fs::read(&native).unwrap(), original);
        let verified = grant
            .verify_with_retry(|| deadline.remaining(), proof, |_| {})
            .unwrap();
        assert_eq!(std::fs::read(&native).unwrap(), original);
        let claim = identity.claim(&state).unwrap();
        assert_eq!(
            google_browser_store::load(&home, &identity, &claim)
                .unwrap()
                .unwrap()
                .as_slice(),
            NEWER
        );
        drop(claim);
        verified.finish_recovery().unwrap();
        assert!(!record.exists());
    }
}

#[test]
fn direct_recovery_never_overwrites_changed_deleted_or_held_credentials() {
    use std::os::unix::fs::PermissionsExt;
    for case in 0..4 {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, _) = setup(root.path());
        let exe = std::env::current_exe().unwrap();
        let deadline = WakeDeadline::after(Duration::from_secs(60));
        let grant = direct_with(&exe, &state, &home, "one", deadline, fresh).unwrap();
        let native = grant.path().to_owned();
        let profile = native
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        drop(grant);
        let record = direct_file(&state);
        let before = std::fs::read(&record).unwrap();
        match case {
            0 => aterm_secrets::fsx::write_atomic(&native, b"different-owner-credential", 0o600)
                .unwrap(),
            1 => std::fs::remove_file(&native).unwrap(),
            2 => std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o644)).unwrap(),
            _ => {}
        }
        let _held = (case == 3)
            .then(|| crate::harness::browser_window::ProfileLock::claim(&profile).unwrap());
        let current = std::fs::read(&native).ok();
        let called = Cell::new(false);
        let result = direct_with(&exe, &state, &home, "one", deadline, |_, _, _, _, _, _| {
            called.set(true);
            assert_eq!(std::fs::read(&native).ok(), current);
            Err(GoogleLoginError::Native(Error::Native))
        });
        assert!(result.is_err());
        assert_eq!(called.get(), case < 2, "case {case}");
        assert_eq!(std::fs::read(&native).ok(), current);
        assert_eq!(std::fs::read(&record).unwrap(), before);
    }
}

#[test]
fn direct_and_pool_records_are_independent_and_cannot_roll_back_a_newer_browser_session() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (state, home, identity) = setup(root.path());
    let exe = std::env::current_exe().unwrap();
    let deadline = WakeDeadline::after(Duration::from_secs(60));
    let pool = run_with(
        &exe,
        &state,
        &home,
        "one",
        staging(&root.path().join("pool-first"), 4),
        deadline,
        super::tests::fresh,
    )
    .unwrap();
    drop(pool);
    let pool_path = candidate_file(&state);
    let pool_bytes = std::fs::read(&pool_path).unwrap();
    let direct = direct_with(&exe, &state, &home, "one", deadline, fresh).unwrap();
    let verified = direct
        .verify_with_retry(|| deadline.remaining(), proof, |_| {})
        .unwrap();
    verified.finish_recovery().unwrap();
    drop(verified);
    assert_eq!(std::fs::read(&pool_path).unwrap(), pool_bytes);
    let claim = identity.claim(&state).unwrap();
    let snapshot = claim.profile().join("session.age");
    let newer_ciphertext = std::fs::read(&snapshot).unwrap();
    drop(claim);
    let pool = run_with(
        &exe,
        &state,
        &home,
        "one",
        staging(&root.path().join("pool-second"), 4),
        deadline,
        |_, _, _, _, _, _| panic!("pool recovery repeated native login"),
    )
    .unwrap();
    let verified = pool
        .verify_with_retry(|| deadline.remaining(), proof, |_| {})
        .unwrap();
    assert_eq!(std::fs::read(&snapshot).unwrap(), newer_ciphertext);
    let claim = identity.claim(&state).unwrap();
    assert_eq!(
        google_browser_store::load(&home, &identity, &claim)
            .unwrap()
            .unwrap()
            .as_slice(),
        NEWER
    );
    drop(claim);
    verified.finish_recovery().unwrap();
    assert!(!pool_path.exists());
    assert!(!direct_file(&state).exists());
}

#[test]
fn a_credential_replaced_before_verification_never_uses_the_old_pending_cookies() {
    for direct in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (state, home, identity) = setup(root.path());
        let exe = std::env::current_exe().unwrap();
        let deadline = WakeDeadline::after(Duration::from_secs(60));
        let grant = if direct {
            direct_with(&exe, &state, &home, "one", deadline, fresh).unwrap()
        } else {
            run_with(
                &exe,
                &state,
                &home,
                "one",
                staging(&root.path().join("pool"), 4),
                deadline,
                super::tests::fresh,
            )
            .unwrap()
        };
        let native = grant.path().to_owned();
        let changed = br#"{"auth_method":"consumer","token":{"access_token":"replacement-token"}}"#;
        aterm_secrets::fsx::write_atomic(&native, changed, 0o600).unwrap();
        assert!(
            grant
                .verify_with_retry(
                    || deadline.remaining(),
                    |_, _, _| panic!("replaced credential reached Google"),
                    |_| {},
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&native).unwrap(), changed);
        let claim = identity.claim(&state).unwrap();
        assert!(
            google_browser_store::load(&home, &identity, &claim)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn version_one_pending_records_keep_native_credentials_but_discard_unscoped_cookies() {
    let token = br#"{"auth_method":"consumer","token":{"access_token":"synthetic-token"}}"#;
    let mut bytes = TOKEN_ONLY_PREFIX.to_vec();
    bytes.extend_from_slice(&[3; 32]);
    bytes.extend_from_slice(&[4; 32]);
    for field in [token.as_slice(), NEWER] {
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    let candidate = decode(&bytes, [3; 32], RecoveryKey([4; 32]))
        .unwrap()
        .unwrap();
    assert_eq!(candidate.token.as_slice(), token);
    assert!(candidate.cookies.is_empty());
    assert!(candidate.previous.is_none());
    assert!(
        decode(&bytes, [5; 32], RecoveryKey([4; 32]))
            .unwrap()
            .is_none()
    );
    bytes.push(0);
    assert!(decode(&bytes, [3; 32], RecoveryKey([4; 32])).is_err());
}
