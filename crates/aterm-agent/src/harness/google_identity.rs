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

    /// A credential source reference, never a password. Vault names resolve through the
    /// vault; `registry_mailbox` has a disjoint internal prefix bound to its exact row.
    /// Even a home-path-shaped vault name is never opened directly as a path.
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
                &[
                    "id",
                    "email",
                    "credential_ref",
                    "registry_mailbox",
                    "verification_email",
                ],
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
            let credential_ref = if let Some(mailbox) = row.get("registry_mailbox") {
                if credential_ref.is_some() {
                    return Err("Google identity has competing credential sources");
                }
                let mailbox = mailbox
                    .as_str()
                    .filter(|v| {
                        v.strip_prefix("mb-").is_some_and(|key| {
                            key.len() == 16
                                && key
                                    .bytes()
                                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                        })
                    })
                    .ok_or("Google identity has an invalid registry mailbox key")?;
                Some(format!("registry-mailbox:{mailbox}"))
            } else {
                credential_ref
            };
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
#[path = "google_identity_tests.rs"]
mod tests;
