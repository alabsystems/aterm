// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Unix only, where `shared_provider::read_staged_bytes` reads owner-private staged bytes;
/// off Unix `shared_provider_private_snapshot_refuses_where_private_evidence_is_unavailable`
/// holds the refusal.
#[cfg(unix)]
#[test]
fn shared_provider_private_snapshot_detects_same_identity_token_replacement_and_creation() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let path = temp.path().join(".credentials.json");
    let absent = PrivateSnapshot::read(path.clone(), true).unwrap();
    std::fs::write(&path, b"synthetic-first").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(absent.check_current().is_err());
    let original = PrivateSnapshot::read(path.clone(), true).unwrap();
    original.check_current().unwrap();
    std::fs::write(&path, b"synthetic-other").unwrap();
    assert!(original.check_current().is_err());
}

/// Off Unix `shared_provider::read_staged_bytes` refuses staged file reads rather than trusting
/// unverified file permissions.
#[cfg(not(unix))]
#[test]
fn shared_provider_private_snapshot_refuses_where_private_evidence_is_unavailable() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let path = temp.path().join(".credentials.json");
    for present in [false, true] {
        if present {
            std::fs::write(&path, b"synthetic-first").unwrap();
        }
        for private in [false, true] {
            let err = match PrivateSnapshot::read(path.clone(), private) {
                Ok(_) => panic!("expected off-Unix refusal (present={present}, private={private})"),
                Err(err) => err,
            };
            assert_eq!(
                err,
                "private shared-login evidence is unavailable on this platform"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn shared_provider_snapshot_refuses_links_and_nonprivate_credentials() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let temp = aterm_tempfile::tempdir().unwrap();
    let path = temp.path().join("credential");
    std::fs::write(&path, b"synthetic").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(PrivateSnapshot::read(path.clone(), true).is_err());
    let link = temp.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(PrivateSnapshot::read(link, false).is_err());
    let alias = temp.path().join("alias");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(PrivateSnapshot::read(alias, false).is_err());
}

#[test]
fn shared_provider_keychain_proof_retains_exact_bytes_and_backend_absence() {
    let current = std::sync::Arc::new(std::sync::Mutex::new(Some(b"synthetic-first".to_vec())));
    let source = current.clone();
    let reader: std::sync::Arc<KeychainReader> = std::sync::Arc::new(move |_, deadline| {
        assert!(deadline > std::time::Instant::now());
        Ok(source
            .lock()
            .unwrap()
            .clone()
            .map(aterm_uds::secret::Secret::from_vec))
    });
    let first = KeychainSnapshot::read(Path::new("/fixture/logins/one+1"), reader.clone()).unwrap();
    first.check_current().unwrap();
    *current.lock().unwrap() = Some(b"synthetic-other".to_vec());
    assert!(first.check_current().is_err());
    *current.lock().unwrap() = None;
    assert!(first.check_current().is_err());
    let absent = KeychainSnapshot::read(Path::new("/fixture/logins/one+1"), reader).unwrap();
    absent.check_current().unwrap();
    *current.lock().unwrap() = Some(b"synthetic-first".to_vec());
    assert!(absent.check_current().is_err());
}

#[test]
fn shared_provider_retry_requires_typed_no_start_not_a_failure_string() {
    use crate::harness::login_drive::Outcome;
    assert!(matches!(
        codex_execution(Outcome::NotRun("not started".into())),
        Execution::NotStarted(_)
    ));
    for outcome in [
        Outcome::Hung,
        Outcome::Unverified("not started".into()),
        Outcome::Failed,
        Outcome::Exited(None),
        Outcome::OffPin,
        Outcome::Refused("not started".into()),
    ] {
        assert!(matches!(codex_execution(outcome), Execution::Unknown(_)));
    }
    assert!(matches!(
        codex_execution(Outcome::SignedIn {
            email: None,
            page: true
        }),
        Execution::Completed
    ));
}
