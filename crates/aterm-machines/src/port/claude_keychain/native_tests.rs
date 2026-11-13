// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

#[test]
fn vendor_account_uses_user_then_os_and_rejects_unsafe_names() {
    let not_called = || -> Result<String, ReadError> { panic!("USER should win") };
    assert_eq!(account_value(Some("person"), not_called), "person");
    assert_eq!(account_value(None, || Ok("os-person".into())), "os-person");
    assert_eq!(
        account_value(Some(""), || Ok("os-person".into())),
        "os-person"
    );
    assert_eq!(
        account_value(None, || Err(ReadError::Unavailable)),
        "claude-code-user"
    );
    for invalid in ["with space", "person@example", "é"] {
        assert_eq!(account_value(Some(invalid), not_called), "claude-code-user");
    }
}

#[test]
fn vendor_service_normalizes_unicode_but_does_not_resolve_paths() {
    let composed = "/private/café/logins/person+1";
    let decomposed = "/private/cafe\u{301}/logins/person+1";
    assert_eq!(
        service(&nfc(composed).unwrap()),
        service(&nfc(decomposed).unwrap())
    );
    assert_ne!(
        service(&nfc(composed).unwrap()),
        service(&nfc("/private/a/../café/logins/person+1").unwrap())
    );
}

fn stage() -> (aterm_tempfile::TempDir, PathBuf) {
    let temp = aterm_tempfile::tempdir().unwrap();
    let parent = temp.path().join("logins");
    aterm_uds::private_file::create_private_dir(&parent).unwrap();
    let stage = parent.join("person+1");
    aterm_uds::private_file::create_private_dir(&stage).unwrap();
    (temp, stage)
}

#[test]
fn scoped_read_accepts_private_stage_and_refuses_replacement_during_query() {
    let (_temp, path) = stage();
    let result = read_in_scope(&path, |_| {
        Ok(Some(SecretBytes::from(b"fixture".as_slice())))
    })
    .unwrap()
    .unwrap();
    assert_eq!(result.as_slice(), b"fixture");
    let result = read_in_scope(&path, |_| {
        std::fs::rename(&path, path.with_file_name("kept-original")).unwrap();
        aterm_uds::private_file::create_private_dir(&path).unwrap();
        Ok(Some(SecretBytes::from(b"fixture".as_slice())))
    });
    assert!(matches!(result, Err(ReadError::Refused)));
}

#[test]
fn loose_or_symlinked_stage_and_parent_never_reach_query() {
    let (_temp, path) = stage();
    let query = |_: &str| -> Result<Option<SecretBytes>, ReadError> {
        panic!("unsafe scope reached query")
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        read_in_scope(&path, query),
        Err(ReadError::Refused)
    ));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let parent = path.parent().unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        read_in_scope(&path, query),
        Err(ReadError::Refused)
    ));
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let moved = path.with_file_name("kept-original");
    std::fs::rename(&path, &moved).unwrap();
    symlink(&moved, &path).unwrap();
    assert!(matches!(
        read_in_scope(&path, query),
        Err(ReadError::Refused)
    ));
}
