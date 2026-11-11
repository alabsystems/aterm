// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn session_is_account_bound_and_rejects_truncation_or_trailing_bytes() {
    let original = Credentials {
        uid: Secret::new("uid".into()),
        access: Secret::new("access".into()),
        refresh: Secret::new("refresh".into()),
        expires: 42,
    };
    let mut bytes = encode("a@example.test", &original).unwrap();
    let decoded = decode("A@example.test", &bytes).unwrap();
    assert_eq!(decoded.refresh.expose(), "refresh");
    assert_eq!(decoded.expires, 42);
    assert!(decode("b@example.test", &bytes).is_err());
    for end in 0..bytes.len() {
        assert!(decode("a@example.test", &bytes[..end]).is_err());
    }
    bytes.push(0);
    assert!(decode("a@example.test", &bytes).is_err());
}

#[cfg(unix)]
#[test]
fn stored_session_is_encrypted_private_and_integrity_checked() {
    use std::os::unix::fs::PermissionsExt;
    let mut nonce = [0; 8];
    aterm_uds::rand::fill(&mut nonce).unwrap();
    let home = std::env::temp_dir().join(format!(
        "aterm-mail-session-{}-{:x}",
        std::process::id(),
        u64::from_le_bytes(nonce)
    ));
    let credentials = Credentials {
        uid: Secret::new("uid".into()),
        access: Secret::new("sensitive-access-token".into()),
        refresh: Secret::new("sensitive-refresh-token".into()),
        expires: 123,
    };
    save(&home, "a@example.test", &credentials).unwrap();
    let path = location(&home, "a@example.test");
    let mut encrypted = std::fs::read(&path).unwrap();
    assert!(!encrypted.windows(9).any(|bytes| bytes == b"sensitive"));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let restored = load(&home, "a@example.test").unwrap().unwrap();
    assert_eq!(restored.refresh.expose(), credentials.refresh.expose());
    let last = encrypted.len() - 1;
    encrypted[last] ^= 1;
    std::fs::write(&path, encrypted).unwrap();
    assert!(load(&home, "a@example.test").is_err());
    std::fs::remove_dir_all(home).unwrap();
}

#[cfg(unix)]
#[test]
fn only_one_connection_can_rotate_an_accounts_tokens() {
    let mut nonce = [0; 8];
    aterm_uds::rand::fill(&mut nonce).unwrap();
    let home = std::env::temp_dir().join(format!(
        "aterm-mail-lease-{}-{:x}",
        std::process::id(),
        u64::from_le_bytes(nonce)
    ));
    let first = acquire(&home, "a@example.test").unwrap();
    assert!(acquire(&home, "A@example.test").is_err());
    let different = acquire(&home, "b@example.test").unwrap();
    drop(first);
    let reopened = acquire(&home, "a@example.test").unwrap();
    drop((different, reopened));
    std::fs::remove_dir_all(home).unwrap();
}
