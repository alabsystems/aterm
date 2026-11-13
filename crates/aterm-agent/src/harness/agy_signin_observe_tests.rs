// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn native_default_observation_is_read_only_and_binds_exact_grant() {
    for case in 0..7 {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = root.path().join(".gemini/antigravity-cli");
        aterm_secrets::fsx::ensure_private_dir(&root.path().join(".gemini")).unwrap();
        aterm_secrets::fsx::ensure_private_dir(&dir).unwrap();
        let path = dir.join("antigravity-oauth-token");
        let bytes = br#"{"auth_method":"consumer","token":{"access_token":"synthetic-access"}}"#;
        aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
        if case == 4 {
            aterm_secrets::fsx::write_atomic(
                &path,
                br#"{"auth_method":"gcp","token":{"access_token":"synthetic-access"}}"#,
                0o600,
            )
            .unwrap();
        }
        if case == 5 {
            let saved = dir.join("saved");
            std::fs::rename(&path, &saved).unwrap();
            std::os::unix::fs::symlink(&saved, &path).unwrap();
        }
        if case == 6 {
            let saved = root.path().join("saved-dir");
            std::fs::rename(&dir, &saved).unwrap();
            std::os::unix::fs::symlink(&saved, &dir).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let called = std::cell::Cell::new(false);
        let result = read_using(
            root.path(),
            "owner@example.com",
            Instant::now() + Duration::from_secs(60),
            |token, expected, _| {
                assert!(!matches!(case, 4..=6));
                called.set(true);
                assert_eq!(token, "synthetic-access");
                assert_eq!(expected, "owner@example.com");
                if case == 1 {
                    return Err(super::super::super::google_profile::Error::Unavailable);
                }
                if case == 2 {
                    aterm_secrets::fsx::write_atomic(
                        &path,
                        b"changed-during-provider-check",
                        0o600,
                    )
                    .unwrap();
                }
                Ok(super::super::super::google_profile::GoogleProfile {
                    email: if case == 3 {
                        "another@example.com"
                    } else {
                        expected
                    }
                    .into(),
                    principal: [7; 32],
                })
            },
        );
        assert_eq!(result.is_ok(), case == 0, "case {case}");
        assert_eq!(called.get(), case < 4);
        if case != 2 {
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        if let Ok(proof) = result {
            assert_eq!(proof.profile().principal, [7; 32]);
            proof.check_current().unwrap();
            aterm_secrets::fsx::write_atomic(&path, b"changed-after-provider-check", 0o600)
                .unwrap();
            assert!(proof.check_current().is_err());
        }
    }
}
