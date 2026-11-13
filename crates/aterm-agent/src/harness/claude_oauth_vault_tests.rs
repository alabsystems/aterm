// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn document() -> Document {
    let this = aterm_secrets::age::Identity::from_secret_bytes([7; 32])
        .recipient()
        .to_bech32();
    let other = aterm_secrets::age::Identity::from_secret_bytes([9; 32])
        .recipient()
        .to_bech32();
    let mut document = Document::new_vault("m2", &this);
    document.add_machine("m13", &other).expect("other Mac");
    document
}

fn credential(access: &str, refresh: &str, expires_at: u64) -> Credential {
    Credential {
        name: "claude-code-m2-oauth".into(),
        value: SecretBytes::from(
            aterm_json::json!({"claudeAiOauth": {
                "accessToken": access,
                "refreshToken": refresh,
                "expiresAt": expires_at,
            }})
            .to_string()
            .into_bytes(),
        ),
        expires_at,
    }
}

#[test]
fn a_saved_oauth_credential_is_encrypted_to_its_own_mac_only() {
    let credential = credential("access", "refresh", 100);
    let document = host_scoped(&document(), "m2", &credential, false)
        .expect("host entry")
        .expect("changed");
    let lockers = document.lockers();
    let own = lockers
        .iter()
        .find(|locker| locker.audience == Audience::Hosts(vec!["m2".into()]))
        .expect("this Mac's locker");
    assert_eq!(own.recipients.len(), 1);
    assert_eq!(
        own.recipients[0],
        aterm_secrets::age::Identity::from_secret_bytes([7; 32])
            .recipient()
            .to_bech32()
    );
    let readable_elsewhere = lockers
        .iter()
        .filter(|locker| locker.audience.includes("m13"))
        .flat_map(|locker| {
            aterm_secrets::text::parse_locker(locker.text.as_slice()).expect("locker")
        })
        .collect::<Vec<_>>();
    assert!(
        readable_elsewhere
            .iter()
            .all(|entry| entry.name != credential.name)
    );
    assert!(
        readable_elsewhere
            .iter()
            .all(|entry| entry.value.as_slice() != credential.value.as_slice())
    );
}

#[test]
fn only_a_verified_login_replaces_a_longer_lived_older_credential() {
    let current = credential("new access", "new refresh", 200);
    let document = host_scoped(&document(), "m2", &current, false)
        .expect("initial")
        .expect("changed");
    assert!(
        host_scoped(&document, "m2", &current, false)
            .expect("current")
            .is_none()
    );
    let older = credential("old access", "old refresh", 100);
    assert!(host_scoped(&document, "m2", &older, false).is_err());
    let newer = credential("next access", "next refresh", 300);
    assert!(
        host_scoped(&document, "m2", &newer, false)
            .expect("newer")
            .is_some()
    );
    assert!(
        host_scoped(&document, "m2", &older, true)
            .expect("a newly verified login may replace the old one")
            .is_some()
    );
}

#[test]
fn only_a_changed_snapshot_needs_another_vault_read() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let first = credential("first", "refresh one", 100);
    assert!(!cached(root.path(), &first));
    mark_cached(root.path(), &first);
    assert!(cached(root.path(), &first));
    let rotated = credential("second", "refresh two", 200);
    assert!(!cached(root.path(), &rotated));
    assert!(!cached(&root.path().join("other"), &first));
}

#[test]
fn only_a_complete_oauth_credential_with_an_expiry_is_saved() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let path = root.path().join(".credentials.json");
    let write = |text: String| {
        std::fs::write(&path, text).expect("credential");
        std::fs::set_permissions(
            &path,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
        )
        .expect("private");
    };
    write(
        aterm_json::json!({"claudeAiOauth": {
            "accessToken": "access", "refreshToken": "refresh", "expiresAt": 100,
        }})
        .to_string(),
    );
    let saved = read_credential("m2", root.path()).expect("complete");
    assert_eq!(saved.name, "claude-code-m2-oauth");
    assert_eq!(saved.expires_at, 100);
    assert!(saved.value.as_slice().starts_with(b"{"));
    for invalid in [
        aterm_json::json!({"claudeAiOauth":{"accessToken":"access","expiresAt":100}}),
        aterm_json::json!({"claudeAiOauth": {
            "accessToken": "access", "refreshToken": "refresh", "expiresAt": 0,
        }}),
    ] {
        write(invalid.to_string());
        assert!(read_credential("m2", root.path()).is_err());
    }
    write("not json".into());
    assert!(read_credential("m2", root.path()).is_err());
}
