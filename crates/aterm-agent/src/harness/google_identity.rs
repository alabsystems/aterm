// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit Google identities and vendor bindings for unattended sign-in.
//!
//! This registry is independent of mailbox routing. It contains secret references,
//! never passwords, and resolves a vendor account only through an explicit binding.
//! Browser launch and credential resolution remain the caller's responsibility.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_toml::{Table, Value};

/// Maximum encoded registry size.
pub const MAX_BYTES: usize = 64 * 1024;
/// Maximum identities or bindings in one registry.
pub const MAX_ROWS: usize = 64;

/// A Google account, distinct from its mailbox and each vendor account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    id: String,
    email: String,
    credential_ref: Option<String>,
    verification_email: Option<String>,
}

impl Identity {
    /// The stable local identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The exact expected Google email, with ASCII case normalized only.
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }

    /// A secrets-registry name, not a password. Even a home-path-shaped name must
    /// be resolved through that registry, never opened directly as a path.
    #[must_use]
    pub fn credential_ref(&self) -> Option<&str> {
        self.credential_ref.as_deref()
    }

    /// Explicit destination for Google's email verification, separate from the Google login.
    #[must_use]
    pub fn verification_email(&self) -> Option<&str> {
        self.verification_email.as_deref()
    }

    /// A machine-local profile namespace independent of vendor, label and password.
    /// Changing the Google email cannot inherit the previous account's session.
    #[must_use]
    fn profile_path(&self, machine_state: &Path) -> PathBuf {
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm.google-identity.v1\0");
        digest.update(self.id.as_bytes());
        digest.update(b"\0");
        digest.update(self.email.as_bytes());
        machine_state
            .join("google-identities")
            .join(aterm_codec::hex::encode_lower(&digest.finalize()))
    }

    /// Claim this identity's private profile without opening a browser or reading cookies.
    /// The guard must outlive the browser. This provides exclusion, not a credential store;
    /// callers must separately establish protected browser storage before signing in.
    ///
    /// # Errors
    /// Refuses a relative state root, unsafe profile directory, filesystem failure,
    /// or a profile already held by another browser or aterm launch.
    pub fn claim(&self, machine_state: &Path) -> Result<Session, SessionError> {
        if !machine_state.is_absolute() {
            return Err(SessionError::RelativeRoot);
        }
        let namespace = machine_state.join("google-identities");
        aterm_types::fs_restricted::ensure_private_dir(&namespace)
            .map_err(SessionError::Storage)?;
        let profile = self.profile_path(machine_state);
        aterm_types::fs_restricted::ensure_private_dir(&profile).map_err(SessionError::Storage)?;
        let lock =
            super::browser_window::ProfileLock::claim(&profile).map_err(SessionError::Claim)?;
        Ok(Session {
            profile,
            _lock: lock,
        })
    }
}

/// Exclusive ownership of one identity's browser profile, released when dropped.
#[derive(Debug)]
pub struct Session {
    profile: PathBuf,
    _lock: super::browser_window::ProfileLock,
}

impl Session {
    /// The claimed profile; no browser or OS permission prompt is opened by the claim.
    #[must_use]
    pub fn profile(&self) -> &Path {
        &self.profile
    }
}

/// Why an identity profile could not be claimed.
#[derive(Debug)]
pub enum SessionError {
    /// A relative root would make identity ownership depend on the working directory.
    RelativeRoot,
    /// The private directory could not be established.
    Storage(std::io::Error),
    /// Another owner holds the profile, or its lock is unavailable.
    Claim(super::browser_window::Refusal),
}

/// One vendor account's explicit Google identity and expected vendor identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    provider: String,
    account: String,
    identity: String,
    expected_email: String,
}

impl Binding {
    /// The provider's own account identity to verify before publishing credentials.
    #[must_use]
    pub fn expected_email(&self) -> &str {
        &self.expected_email
    }
}

/// An admitted registry; missing bindings never fall back to another Google account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    identities: BTreeMap<String, Identity>,
    bindings: BTreeMap<(String, String), Binding>,
}

fn word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn text(table: &Table, key: &str) -> Result<String, &'static str> {
    table
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or("Google identity registry has a missing or invalid text field")
}

fn keys(table: &Table, allowed: &[&str]) -> Result<(), &'static str> {
    if table.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err("Google identity registry contains an unknown field")
    }
}

fn email(table: &Table, key: &str) -> Result<String, &'static str> {
    let value = text(table, key)?;
    if super::accounts::email_refusal(&value).is_some() {
        return Err("Google identity registry contains an invalid email");
    }
    Ok(value.to_ascii_lowercase())
}

fn rows<'a>(table: &'a Table, key: &str) -> Result<Vec<&'a Table>, &'static str> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or("Google identity registry requires table arrays")?;
    if array.len() > MAX_ROWS {
        return Err("Google identity registry has too many rows");
    }
    array
        .iter()
        .map(|v| {
            v.as_table()
                .ok_or("Google identity registry requires table rows")
        })
        .collect()
}

impl Registry {
    /// Parse explicit identities and vendor bindings. Diagnostics never echo source data.
    ///
    /// # Errors
    /// Rejects oversized or malformed TOML, unknown fields, invalid identifiers, duplicate
    /// identities or bindings, and references to identities which do not exist.
    pub fn parse(source: &str) -> Result<Self, &'static str> {
        if source.len() > MAX_BYTES {
            return Err("Google identity registry is too large");
        }
        let root: Table = aterm_toml::from_str(source)
            .map_err(|_| "Google identity registry is not valid TOML")?;
        keys(&root, &["identities", "bindings"])?;
        let mut registry = Self::default();
        let mut emails = BTreeSet::new();
        for row in rows(&root, "identities")? {
            keys(
                row,
                &["id", "email", "credential_ref", "verification_email"],
            )?;
            let id = text(row, "id")?;
            if !word(&id) {
                return Err("Google identity registry contains an invalid identity ID");
            }
            let email = email(row, "email")?;
            let verification_email = row
                .get("verification_email")
                .map(|_| self::email(row, "verification_email"))
                .transpose()?;
            let credential_ref = row
                .get("credential_ref")
                .map(|_| text(row, "credential_ref"))
                .transpose()?;
            if credential_ref
                .as_deref()
                .is_some_and(|reference| !aterm_secrets::text::valid_entry_name(reference))
            {
                return Err("Google identity registry contains an invalid secret reference");
            }
            if !emails.insert(email.clone()) || registry.identities.contains_key(&id) {
                return Err("Google identity registry repeats an identity or Google email");
            }
            registry.identities.insert(
                id.clone(),
                Identity {
                    id,
                    email,
                    credential_ref,
                    verification_email,
                },
            );
        }
        for row in rows(&root, "bindings")? {
            keys(row, &["provider", "account", "identity", "expected_email"])?;
            let provider = text(row, "provider")?;
            let account = text(row, "account")?;
            let identity = text(row, "identity")?;
            if !word(&provider) || !word(&account) {
                return Err("Google identity registry contains an invalid vendor account key");
            }
            if !registry.identities.contains_key(&identity) {
                return Err("Google identity binding names an unknown identity");
            }
            let expected_email = email(row, "expected_email")?;
            let key = (provider.clone(), account.clone());
            if registry
                .bindings
                .insert(
                    key,
                    Binding {
                        provider,
                        account,
                        identity,
                        expected_email,
                    },
                )
                .is_some()
            {
                return Err("Google identity registry repeats a vendor account binding");
            }
        }
        Ok(registry)
    }

    /// Resolve a vendor account without aliases, mailbox inference or a default identity.
    #[must_use]
    pub fn resolve(&self, provider: &str, account: &str) -> Option<(&Identity, &Binding)> {
        let binding = self
            .bindings
            .get(&(provider.to_owned(), account.to_owned()))?;
        Some((self.identities.get(&binding.identity)?, binding))
    }

    /// Fingerprint the configuration an in-flight login must still match before commit.
    /// A credential value or browser-session change needs its own generation check.
    #[must_use]
    pub fn binding_revision(&self, provider: &str, account: &str) -> Option<[u8; 32]> {
        let (identity, binding) = self.resolve(provider, account)?;
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm.google-binding.v1\0");
        for field in [
            identity.id(),
            identity.email(),
            identity.credential_ref().unwrap_or_default(),
            identity.verification_email().unwrap_or_default(),
            binding.provider.as_str(),
            binding.account.as_str(),
            binding.expected_email(),
        ] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
        Some(digest.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY: &str = r#"
[[identities]]
id = "primary"
email = "Person.Name+ai@gmail.com"
credential_ref = "google-primary"
[[bindings]]
provider = "kimi-global"
account = "work"
identity = "primary"
expected_email = "person.name+ai@gmail.com"
[[bindings]]
provider = "claude"
account = "work"
identity = "primary"
expected_email = "vendor-alias@example.com"
[[bindings]]
provider = "agy"
account = "work"
identity = "primary"
expected_email = "person.name+ai@gmail.com"
"#;

    #[test]
    fn verification_mailbox_is_explicit_and_changes_the_binding_not_the_google_profile() {
        let initial = Registry::parse(REGISTRY).unwrap();
        let with_mail = REGISTRY.replace(
            "credential_ref = \"google-primary\"",
            "credential_ref = \"google-primary\"\nverification_email = \"recovery@example.com\"",
        );
        let configured = Registry::parse(&with_mail).unwrap();
        let before = initial.resolve("kimi-global", "work").unwrap().0;
        let after = configured.resolve("kimi-global", "work").unwrap().0;
        assert_eq!(before.verification_email(), None);
        assert_eq!(after.verification_email(), Some("recovery@example.com"));
        assert_eq!(
            before.profile_path(Path::new("/state")),
            after.profile_path(Path::new("/state"))
        );
        assert_ne!(
            initial.binding_revision("kimi-global", "work"),
            configured.binding_revision("kimi-global", "work")
        );
        assert!(
            Registry::parse(&with_mail.replace("recovery@example.com", "not an email")).is_err()
        );
    }

    #[test]
    fn explicit_vendor_bindings_share_only_the_selected_google_identity() {
        let registry = Registry::parse(REGISTRY).unwrap();
        let (kimi, _) = registry.resolve("kimi-global", "work").unwrap();
        let (claude, binding) = registry.resolve("claude", "work").unwrap();
        let (agy, _) = registry.resolve("agy", "work").unwrap();
        assert_eq!(kimi, claude);
        assert_eq!(kimi, agy);
        assert_eq!(kimi.email(), "person.name+ai@gmail.com");
        assert_eq!(kimi.credential_ref(), Some("google-primary"));
        assert_eq!(binding.expected_email(), "vendor-alias@example.com");
        assert!(registry.resolve("kimi-mainland", "work").is_none());
        assert!(registry.resolve("claude", "other").is_none());
    }

    #[test]
    fn references_follow_the_existing_secret_registry_grammar() {
        for reference in [
            "google-primary",
            "google-login.toml",
            "~/.config/aterm/google-login",
        ] {
            let registry = Registry::parse(&REGISTRY.replace("google-primary", reference)).unwrap();
            assert_eq!(
                registry.resolve("agy", "work").unwrap().0.credential_ref(),
                Some(reference)
            );
        }
        for reference in [
            "../private-secret",
            "/tmp/login",
            "~/../login",
            "~/.secrets/login",
        ] {
            assert!(Registry::parse(&REGISTRY.replace("google-primary", reference)).is_err());
        }
    }

    #[test]
    fn profile_identity_survives_credential_rotation_but_not_account_replacement() {
        let root = Path::new("/machine/state");
        let initial = Registry::parse(REGISTRY).unwrap();
        let changed_secret =
            Registry::parse(&REGISTRY.replace("google-primary", "google-new")).unwrap();
        let changed_email =
            Registry::parse(&REGISTRY.replace("Person.Name+ai@gmail.com", "other@gmail.com"))
                .unwrap();
        let profile = |r: &Registry| r.resolve("agy", "work").unwrap().0.profile_path(root);
        assert_eq!(profile(&initial), profile(&changed_secret));
        assert_ne!(profile(&initial), profile(&changed_email));
        assert!(!profile(&initial).to_string_lossy().contains("gmail"));
        assert_ne!(
            initial.binding_revision("agy", "work"),
            changed_secret.binding_revision("agy", "work")
        );
        let changed_vendor =
            Registry::parse(&REGISTRY.replace("vendor-alias@example.com", "other@example.com"))
                .unwrap();
        assert_ne!(
            initial.binding_revision("claude", "work"),
            changed_vendor.binding_revision("claude", "work")
        );
        assert_eq!(
            initial.binding_revision("agy", "work"),
            changed_vendor.binding_revision("agy", "work")
        );
    }

    #[test]
    fn vendors_exclude_each_other_until_the_identity_owner_releases_its_profile() {
        let root = aterm_tempfile::tempdir().unwrap();
        let registry = Registry::parse(REGISTRY).unwrap();
        let (kimi, _) = registry.resolve("kimi-global", "work").unwrap();
        let (claude, _) = registry.resolve("claude", "work").unwrap();
        let owner = kimi.claim(root.path()).unwrap();
        assert_eq!(owner.profile(), kimi.profile_path(root.path()));
        assert!(matches!(
            claude.claim(root.path()),
            Err(SessionError::Claim(_))
        ));
        drop(owner);
        let reloaded = Registry::parse(REGISTRY).unwrap();
        let recovered = reloaded
            .resolve("agy", "work")
            .unwrap()
            .0
            .claim(root.path())
            .unwrap();
        assert_eq!(recovered.profile(), kimi.profile_path(root.path()));
        assert!(matches!(
            kimi.claim(Path::new("relative")),
            Err(SessionError::RelativeRoot)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn an_identity_profile_cannot_be_redirected_by_a_symlink() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let root = aterm_tempfile::tempdir().unwrap();
        let outside = aterm_tempfile::tempdir().unwrap();
        let registry = Registry::parse(REGISTRY).unwrap();
        let identity = registry.resolve("agy", "work").unwrap().0;
        let session = identity.claim(root.path()).unwrap();
        let profile = session.profile().to_owned();
        assert_eq!(
            std::fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(session);
        std::fs::remove_dir_all(&profile).unwrap();
        symlink(outside.path(), &profile).unwrap();
        assert!(matches!(
            identity.claim(root.path()),
            Err(SessionError::Storage(_))
        ));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn ambiguous_bindings_and_implicit_secret_values_are_refused_without_echoing_them() {
        for bad in [
            REGISTRY.replace("credential_ref =", "password ="),
            REGISTRY.replace("google-primary", "../private-secret"),
            REGISTRY.replace("identity = \"primary\"", "identity = \"missing\""),
            format!("{REGISTRY}\n[[identities]]\nid='second'\nemail='PERSON.NAME+AI@gmail.com'"),
            format!(
                "{REGISTRY}\n[[bindings]]\nprovider='agy'\naccount='work'\nidentity='primary'\nexpected_email='other@example.com'"
            ),
            format!("{REGISTRY}\n[[identities]]\nid='../escape'\nemail='other@example.com'"),
            format!("password = 'never-echo-this-secret'\n{REGISTRY}"),
        ] {
            let error = Registry::parse(&bad).unwrap_err();
            assert!(!error.contains("never-echo"));
            assert!(!error.contains("@"));
        }
        assert!(Registry::parse(&" ".repeat(MAX_BYTES + 1)).is_err());
    }
}
