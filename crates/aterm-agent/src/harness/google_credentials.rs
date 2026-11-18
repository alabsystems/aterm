// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Google credentials from an explicitly bound vault entry or canonical Gmail registry row.
//! No address guess, local file or unrelated mailbox supplies a Google credential.

use std::path::Path;

use aterm_secrets::SecretBytes;
#[cfg(any(unix, test))]
use aterm_toml::{Table, Value};

use super::google_identity::Identity;

/// Maximum size of one Google credential bundle, before parsing.
pub const MAX_BUNDLE_BYTES: usize = 16 * 1024;

/// A credential failure, without any secret, source text or account address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    MissingReference,
    MissingVault,
    MissingEntry,
    InvalidBundle,
    WrongIdentity,
    WrongOrigin,
    Changed,
    Unavailable,
    Unsupported,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MissingReference => "Google identity has no registered credential reference",
            Self::MissingVault => "Google credential registry is not configured",
            Self::MissingEntry => "registered Google credential is unavailable",
            Self::InvalidBundle => "registered Google credential bundle is invalid",
            Self::WrongIdentity => "Google credential belongs to another identity",
            Self::WrongOrigin => "Google credentials cannot be used on this origin",
            Self::Changed => "Google credential or its authorization changed during sign-in",
            Self::Unavailable => "Google credential registry could not be read",
            Self::Unsupported => "Google credential registry is unavailable on this platform",
        })
    }
}

impl std::error::Error for Error {}

/// Strings left in a parsed bundle are wiped, including on an invalid field or identity.
#[cfg(any(unix, test))]
struct SecretTable(Table);

#[cfg(any(unix, test))]
impl Drop for SecretTable {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        for (key, value) in std::mem::replace(&mut self.0, Table::new()) {
            drop(SecretBytes::from(key));
            pending.push(value);
        }
        while let Some(value) = pending.pop() {
            match value {
                Value::String(text) => drop(SecretBytes::from(text)),
                Value::Array(values) => pending.extend(values),
                Value::Table(table) => {
                    for (key, value) in table {
                        drop(SecretBytes::from(key));
                        pending.push(value);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(any(unix, test))]
impl SecretTable {
    fn take_secret(&mut self, name: &str) -> Result<SecretBytes, Error> {
        if self.0.get(name).and_then(Value::as_str).is_none() {
            return Err(Error::InvalidBundle);
        }
        match self.0.remove(name) {
            Some(Value::String(text)) => Ok(SecretBytes::from(text)),
            _ => Err(Error::InvalidBundle),
        }
    }
}

struct Bundle {
    password: SecretBytes,
    authenticator: Option<SecretBytes>,
}

#[cfg(any(unix, test))]
impl Bundle {
    fn parse(value: SecretBytes, expected_email: &str) -> Result<Self, Error> {
        if value.len() > MAX_BUNDLE_BYTES {
            return Err(Error::InvalidBundle);
        }
        let text = value.as_str().ok_or(Error::InvalidBundle)?;
        let mut table = SecretTable(aterm_toml::from_str(text).map_err(|_| Error::InvalidBundle)?);
        if table.0.keys().any(|key| {
            !matches!(
                key.as_str(),
                "provider" | "email" | "password" | "totp_secret"
            )
        }) || table.0.get("provider").and_then(Value::as_str) != Some("google")
        {
            return Err(Error::InvalidBundle);
        }
        let email = table
            .0
            .get("email")
            .and_then(Value::as_str)
            .ok_or(Error::InvalidBundle)?;
        if super::accounts::email_refusal(email).is_some() {
            return Err(Error::InvalidBundle);
        }
        if !email.eq_ignore_ascii_case(expected_email) {
            return Err(Error::WrongIdentity);
        }
        let password = table.take_secret("password")?;
        if password.is_empty()
            || password.len() > 4096
            || password
                .as_str()
                .is_none_or(|text| text.chars().any(char::is_control))
        {
            return Err(Error::InvalidBundle);
        }
        let authenticator = if table.0.get("totp_secret").is_some() {
            let secret = table.take_secret("totp_secret")?;
            if secret
                .as_str()
                .and_then(|text| super::headless_oauth::generate_totp_at(text, 0))
                .is_none()
            {
                return Err(Error::InvalidBundle);
            }
            Some(secret)
        } else {
            None
        };
        Ok(Self {
            password,
            authenticator,
        })
    }
}

/// The parts of a registry mailbox a Google sign-in is handed: its password, and its TOTP seed
/// where it lists one.
#[cfg(unix)]
fn registry_parts(
    mailbox: &super::alab_roster::LabMailbox,
) -> Result<&'static [super::alab_roster::Part], Error> {
    use super::alab_roster::Part;
    if !mailbox.held.contains(&Part::Password) {
        return Err(Error::MissingEntry);
    }
    Ok(if mailbox.held.contains(&Part::Totp) {
        &[Part::Password, Part::Totp]
    } else {
        &[Part::Password]
    })
}

/// The registry mailbox's bundle, revealed (and journalled) once: [`Credentials::check_current`]
/// asks later, with nothing revealed, whether it is still the registry's
/// ([`super::alab_roster::same_bound`]).
#[cfg(unix)]
fn registry_bundle(mailbox: &super::alab_roster::LabMailbox) -> Result<Bundle, Error> {
    use super::alab_roster::{self, Part};
    let parts = registry_parts(mailbox)?;
    let values = alab_roster::reveal_bound(mailbox, parts).map_err(|_| Error::Unavailable)?;
    let password = &values[0];
    if password.is_empty() || password.len() > 4096 || password.chars().any(char::is_control) {
        return Err(Error::InvalidBundle);
    }
    let authenticator = if mailbox.held.contains(&Part::Totp) {
        let seed = &values[1];
        if super::headless_oauth::generate_totp_at(seed, 0).is_none() {
            return Err(Error::InvalidBundle);
        }
        Some(SecretBytes::from(seed.as_bytes()))
    } else {
        None
    };
    Ok(Bundle {
        password: SecretBytes::from(password.as_bytes()),
        authenticator,
    })
}

/// Loaded Google credentials with a retained authenticated-vault observation.
/// This is not proof of provider acceptance or permission to use an account's budget.
pub struct Credentials {
    email: String,
    bundle: Bundle,
    #[cfg(unix)]
    paths: aterm_secrets::client::Paths,
    #[cfg(unix)]
    snapshot: Option<aterm_secrets::client::EntrySnapshot>,
    /// The registry mailbox the bundle was revealed from ([`registry_bundle`]).
    #[cfg(unix)]
    registry: Option<super::alab_roster::LabMailbox>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleCredentials").finish_non_exhaustive()
    }
}

impl Credentials {
    /// Load exactly the referenced registered entry, requiring `provider = "google"`
    /// and the identity's exact email. Never reads an unregistered local fallback.
    /// No network request, credential request or permission dialog is initiated.
    ///
    /// # Errors
    /// Missing, unauthenticated, malformed, mismatched or unavailable credentials.
    pub fn load(identity: &Identity, home: &Path) -> Result<Self, Error> {
        let reference = identity.credential_ref().ok_or(Error::MissingReference)?;
        #[cfg(unix)]
        {
            use aterm_secrets::client::{self, Paths};
            let paths = Paths::for_home(home);
            if let Some(key) = reference.strip_prefix("registry-mailbox:") {
                let mut found = super::alab_roster::mail()
                    .into_iter()
                    .filter(|m| m.pk == key);
                let mailbox = found.next().ok_or(Error::MissingEntry)?;
                if found.next().is_some()
                    || mailbox.service.as_deref() != Some("gmail")
                    || !mailbox.login.eq_ignore_ascii_case(identity.email())
                {
                    return Err(Error::WrongIdentity);
                }
                let bundle = registry_bundle(&mailbox)?;
                return Ok(Self {
                    email: identity.email().to_owned(),
                    bundle,
                    paths,
                    snapshot: None,
                    registry: Some(mailbox),
                });
            }
            let config = client::config(&paths)
                .map_err(|_| Error::Unavailable)?
                .ok_or(Error::MissingVault)?;
            let entry = client::read_verified_entry(&paths, &config, reference)
                .map_err(|_| Error::Unavailable)?
                .ok_or(Error::MissingEntry)?;
            let (value, snapshot) = entry.into_parts();
            let bundle = Bundle::parse(value, identity.email())?;
            Ok(Self {
                email: identity.email().to_owned(),
                bundle,
                paths,
                snapshot: Some(snapshot),
                registry: None,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (reference, home);
            Err(Error::Unsupported)
        }
    }

    /// The Google identity this bundle is explicitly bound to.
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }

    /// Recheck the complete credential and its vault authorization before publication.
    /// Advances the observation floor across unrelated updates. This is a fresh local
    /// observation, not a lease excluding a later credential change. A registry mailbox's
    /// bundle is rechecked with nothing revealed: under the same binding, its cells must still
    /// open to the values this bundle holds (`alab_roster::same_bound`), so a drive that
    /// rechecks at every step journals one reveal, its load's.
    ///
    /// # Errors
    /// The credential, audience, trust context or observed history changed, or cannot be read.
    pub fn check_current(&mut self) -> Result<(), Error> {
        #[cfg(unix)]
        {
            if let Some(mailbox) = &self.registry {
                let parts = registry_parts(mailbox).map_err(|_| Error::Changed)?;
                let held: Vec<&[u8]> = std::iter::once(&self.bundle.password)
                    .chain(&self.bundle.authenticator)
                    .map(SecretBytes::as_slice)
                    .collect();
                return match super::alab_roster::same_bound(mailbox, parts, &held) {
                    Ok(true) => Ok(()),
                    Ok(false) | Err(_) => Err(Error::Changed),
                };
            }
            let entry = self
                .snapshot
                .as_ref()
                .ok_or(Error::Changed)?
                .read(&self.paths)
                .map_err(|_| Error::Unavailable)?
                .ok_or(Error::Changed)?;
            self.snapshot = Some(entry.snapshot().clone());
            Ok(())
        }
        #[cfg(not(unix))]
        Err(Error::Unsupported)
    }

    /// Expose credentials only after checking Google's origin and current vault authority.
    /// The browser operation must independently check its current origin inside the page:
    /// a previously observed URL cannot exclude navigation between browser commands.
    ///
    /// # Errors
    /// Wrong origin, changed authorization or an unavailable current credential.
    pub fn with_google_page<T>(
        &mut self,
        page_url: &str,
        apply: impl FnOnce(&str, Option<&str>) -> T,
    ) -> Result<T, Error> {
        if super::login_drive::https_host(page_url).as_deref() != Some("accounts.google.com") {
            return Err(Error::WrongOrigin);
        }
        self.check_current()?;
        let password = self.bundle.password.as_str().ok_or(Error::InvalidBundle)?;
        let authenticator = self
            .bundle
            .authenticator
            .as_ref()
            .and_then(SecretBytes::as_str);
        Ok(apply(password, authenticator))
    }
}

#[cfg(test)]
#[path = "google_credentials_tests.rs"]
mod tests;
