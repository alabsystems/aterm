// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn missing_unknown_and_unavailable_identity_evidence_are_distinct() {
    let dir = aterm_tempfile::tempdir().unwrap();
    assert_eq!(
        identity_evidence_in_home(dir.path(), "expected@example.test").unwrap(),
        IdentityEvidence::Missing
    );
    let plain = dir.path().join(".claude.json");
    std::fs::write(&plain, b"{}").unwrap();
    assert_eq!(
        identity_evidence_in_home(dir.path(), "expected@example.test").unwrap(),
        IdentityEvidence::Present {
            observation: IdentityObservation::default(),
            email_digest: None
        }
    );
    std::fs::write(&plain, b"not JSON").unwrap();
    assert!(identity_evidence_in_home(dir.path(), "expected@example.test").is_err());
    std::fs::remove_file(&plain).unwrap();
    std::fs::create_dir(&plain).unwrap();
    assert!(identity_evidence_in_home(dir.path(), "expected@example.test").is_err());
}
#[test]
fn email_only_identity_evidence_binds_normalized_email_without_principal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude.json");
    let write = |email: &str| {
        std::fs::write(
            &path,
            aterm_json::to_vec(&aterm_json::json!({"oauthAccount":{"emailAddress":email}}))
                .unwrap(),
        )
        .unwrap()
    };
    write("First@Example.Test");
    let first = identity_evidence_in_home(dir.path(), "").unwrap();
    write(" first@example.test ");
    assert_eq!(identity_evidence_in_home(dir.path(), "").unwrap(), first);
    write("second@example.test");
    assert_ne!(identity_evidence_in_home(dir.path(), "").unwrap(), first);
    assert!(!format!("{first:?}").contains("example"));
}
#[test]
#[cfg(unix)]
fn dangling_selected_profile_is_unavailable_not_missing() {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(dir.path().join("absent"), dir.path().join(".claude.json")).unwrap();
    assert!(identity_evidence_in_home(dir.path(), "expected@example.test").is_err());
}
