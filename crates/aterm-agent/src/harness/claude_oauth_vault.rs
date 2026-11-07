// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-scoped snapshots of Claude Code's OAuth credential after a verified account binding.
//! The vault's ordinary entry-name prohibition still keeps Claude's live credential path out of
//! handovers; this separate entry is encrypted only to the Mac that completed the sign-in.

use aterm_secrets::SecretBytes;
use aterm_secrets::client::Paths;
use aterm_secrets::text::{Audience, Document};
use std::path::Path;
use std::time::{Duration, Instant};

const PUBLISH_WITHIN: Duration = Duration::from_secs(30);

struct Credential {
    name: String,
    value: SecretBytes,
    expires_at: u64,
}

/// Save a routine credential refresh for this Mac alone, never an older snapshot over a newer one.
pub fn save(label: &str, home: &Path) -> Result<String, String> {
    save_with_policy(label, home, false)
}

/// Save a credential for this Mac alone. A newly verified login may replace an older login whose
/// access token happened to live longer; a routine refresh may not.
pub fn save_with_policy(
    label: &str,
    home: &Path,
    verified_replacement: bool,
) -> Result<String, String> {
    let paths = Paths::live().map_err(|error| error.to_string())?;
    save_at(&paths, label, home, verified_replacement)
}

fn save_at(
    paths: &Paths,
    label: &str,
    home: &Path,
    verified_replacement: bool,
) -> Result<String, String> {
    let credential = read_credential(label, home)?;
    let name = credential.name.clone();
    if cached(home, &credential) {
        return Ok(name);
    }
    let admin = aterm_secrets::client::admin_open(paths).map_err(|error| error.to_string())?;
    let host = aterm_secrets::client::machine_named_in_vault(paths, &admin, &admin.config.host)
        .map_err(|error| error.to_string())?;
    let Some(doc) = host_scoped(&admin.doc, &host, &credential, verified_replacement)? else {
        mark_cached(home, &credential);
        return Ok(name);
    };
    let deadline = Instant::now()
        .checked_add(PUBLISH_WITHIN)
        .ok_or("the OAuth snapshot publication deadline is invalid")?;
    aterm_secrets::client::admin_publish_conditional_until(paths, &admin, &doc, deadline)
        .map_err(|error| error.to_string())?;
    mark_cached(home, &credential);
    Ok(name)
}

type Snapshots = std::collections::BTreeMap<(std::path::PathBuf, String), ([u8; 32], Instant)>;

fn snapshots() -> &'static std::sync::Mutex<Snapshots> {
    static SNAPSHOTS: std::sync::OnceLock<std::sync::Mutex<Snapshots>> = std::sync::OnceLock::new();
    SNAPSHOTS.get_or_init(|| std::sync::Mutex::new(Snapshots::new()))
}

fn digest(credential: &Credential) -> [u8; 32] {
    aterm_digest::Sha256::digest(credential.value.as_slice())
}

fn cached(home: &Path, credential: &Credential) -> bool {
    snapshots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(home.to_path_buf(), credential.name.clone()))
        .is_some_and(|(held, at)| {
            *held == digest(credential) && at.elapsed() < Duration::from_secs(60)
        })
}

fn mark_cached(home: &Path, credential: &Credential) {
    snapshots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            (home.to_path_buf(), credential.name.clone()),
            (digest(credential), Instant::now()),
        );
}

fn host_scoped(
    current: &Document,
    host: &str,
    credential: &Credential,
    verified_replacement: bool,
) -> Result<Option<Document>, String> {
    let audience = Audience::Hosts(vec![host.to_owned()]);
    if let Some(held) = current.entry(&audience, &credential.name) {
        if aterm_digest::ct_eq(held.value.as_slice(), credential.value.as_slice()) {
            return Ok(None);
        }
        if !verified_replacement && credential.expires_at < credential_expiry(&held.value)? {
            return Err(
                "the vault holds a newer OAuth credential; the older snapshot was not published"
                    .into(),
            );
        }
    }
    let mut next = current.clone();
    next.set_entry(&audience, &credential.name, credential.value.as_slice())
        .map_err(|error| error.to_string())?;
    Ok(Some(next))
}

fn credential_expiry(bytes: &SecretBytes) -> Result<u64, String> {
    let parsed: aterm_json::Value = aterm_json::from_slice(bytes.as_slice())
        .map_err(|_| "the saved OAuth credential is not valid JSON".to_owned())?;
    let expiry = parsed["claudeAiOauth"]["expiresAt"].as_u64();
    super::cli::shared_provider::wipe_json(parsed);
    expiry
        .filter(|expiry| *expiry > 0)
        .ok_or_else(|| "the saved OAuth credential has no valid expiry".to_owned())
}

fn read_credential(label: &str, home: &Path) -> Result<Credential, String> {
    let path =
        crate::ai::connector::stored_login_file(super::accounts::Cli::Claude.connector(), home)
            .ok_or_else(|| "the verified Claude login has no credential file".to_owned())?;
    let bytes = aterm_secrets::fsx::read_private(&path, aterm_secrets::text::MAX_VALUE)
        .map_err(|_| "the verified Claude credential cannot be read privately".to_owned())?
        .ok_or_else(|| "the verified Claude credential disappeared".to_owned())?;
    let parsed: aterm_json::Value = aterm_json::from_slice(bytes.as_slice())
        .map_err(|_| "the verified Claude credential is not valid JSON".to_owned())?;
    let oauth = &parsed["claudeAiOauth"];
    let complete = oauth.is_object()
        && ["accessToken", "refreshToken"]
            .into_iter()
            .all(|key| oauth[key].as_str().is_some_and(|token| !token.is_empty()));
    let expires_at = oauth["expiresAt"].as_u64().filter(|expiry| *expiry > 0);
    super::cli::shared_provider::wipe_json(parsed);
    if !complete {
        return Err("the verified Claude credential has no complete OAuth token pair".into());
    }
    let expires_at = expires_at
        .ok_or_else(|| "the verified Claude credential has no valid OAuth expiry".to_owned())?;
    Ok(Credential {
        name: format!("claude-code-{label}-oauth"),
        value: bytes,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
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
}
