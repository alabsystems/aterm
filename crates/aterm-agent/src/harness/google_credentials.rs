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

#[cfg(unix)]
fn registry_bundle(mailbox: &super::alab_roster::LabMailbox) -> Result<Bundle, Error> {
    use super::alab_roster::{self, Part};
    if !mailbox.held.contains(&Part::Password) {
        return Err(Error::MissingEntry);
    }
    let parts: &[Part] = if mailbox.held.contains(&Part::Totp) {
        &[Part::Password, Part::Totp]
    } else {
        &[Part::Password]
    };
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
    /// observation, not a lease excluding a later credential change.
    ///
    /// # Errors
    /// The credential, audience, trust context or observed history changed, or cannot be read.
    pub fn check_current(&mut self) -> Result<(), Error> {
        #[cfg(unix)]
        {
            if let Some(mailbox) = &self.registry {
                let current = registry_bundle(mailbox).map_err(|_| Error::Changed)?;
                if current.password.as_slice() != self.bundle.password.as_slice()
                    || current.authenticator.as_ref().map(|s| s.as_slice())
                        != self.bundle.authenticator.as_ref().map(|s| s.as_slice())
                {
                    return Err(Error::Changed);
                }
                return Ok(());
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
mod tests {
    use super::*;

    const VALUE: &str = "provider='google'\nemail='person@example.com'\npassword='fixture-only-password'\ntotp_secret='JBSWY3DPEHPK3PXP'\n";

    #[test]
    fn bundles_bind_provider_and_identity_without_mailbox_fallbacks() {
        assert!(Bundle::parse(SecretBytes::from(VALUE.as_bytes()), "PERSON@example.com").is_ok());
        for value in [
            VALUE.replace("provider='google'", "provider='proton'"),
            VALUE.replace("password=", "mailbox_password="),
            VALUE.replace("fixture-only-password", ""),
            VALUE.replace("JBSWY3DPEHPK3PXP", "not-a-base32-secret!"),
            "fixture-only-password".into(),
        ] {
            assert!(matches!(
                Bundle::parse(SecretBytes::from(value), "person@example.com"),
                Err(Error::InvalidBundle)
            ));
        }
        assert!(matches!(
            Bundle::parse(SecretBytes::from(VALUE.as_bytes()), "another@example.com"),
            Err(Error::WrongIdentity)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_registry_credentials_bind_service_identity_and_current_secret() {
        use super::super::alab_roster::{self, LabMailbox, Part};
        let source = "[[identities]]\nid='one'\nemail='person@example.com'\nregistry_mailbox='mb-0123456789abcdef'\n[[bindings]]\nprovider='agy'\naccount='work'\nidentity='one'\nexpected_email='person@example.com'\n";
        let registry = super::super::google_identity::Registry::parse(source).unwrap();
        let identity = registry.resolve("agy", "work").unwrap().0;
        let root = aterm_tempfile::tempdir().unwrap();
        for case in 0..6 {
            let mailbox = LabMailbox {
                pk: "mb-0123456789abcdef".into(),
                login: if case == 2 {
                    "another@example.com"
                } else {
                    "person@example.com"
                }
                .into(),
                service: Some(if case == 1 { "proton" } else { "gmail" }.into()),
                webmail: None,
                receives: Vec::new(),
                held: if case == 3 {
                    Vec::new()
                } else {
                    vec![Part::Password, Part::Totp]
                },
            };
            let rows = if case == 4 {
                vec![mailbox.clone(), mailbox.clone()]
            } else {
                vec![mailbox.clone()]
            };
            alab_roster::with_mail(
                rows,
                vec![
                    ((&mailbox.pk, Part::Password), "fixture-registry-password"),
                    ((&mailbox.pk, Part::Totp), "JBSWY3DPEHPK3PXP"),
                ],
                || {
                    let credentials = Credentials::load(identity, root.path());
                    if (1..=4).contains(&case) {
                        assert!(credentials.is_err(), "case {case}");
                        return;
                    }
                    let mut credentials = credentials.unwrap();
                    assert!(
                        credentials
                            .with_google_page("https://not-google.example/", |_, _| panic!(
                                "wrong origin exposed credentials"
                            ))
                            .is_err()
                    );
                    credentials
                        .with_google_page("https://accounts.google.com/login", |password, seed| {
                            assert_eq!(password, "fixture-registry-password");
                            assert_eq!(seed, Some("JBSWY3DPEHPK3PXP"));
                        })
                        .unwrap();
                    let changed = if case == 5 {
                        "changed-registry-password"
                    } else {
                        "fixture-registry-password"
                    };
                    let mut new_binding = mailbox.clone();
                    if case == 0 {
                        new_binding.login = "replacement@example.com".into();
                    }
                    alab_roster::with_mail(
                        vec![new_binding],
                        vec![
                            ((&mailbox.pk, Part::Password), changed),
                            ((&mailbox.pk, Part::Totp), "JBSWY3DPEHPK3PXP"),
                        ],
                        || {
                            assert!(matches!(credentials.check_current(), Err(Error::Changed)));
                            assert!(
                                credentials
                                    .with_google_page(
                                        "https://accounts.google.com/",
                                        |_, _| panic!("changed credential reached browser")
                                    )
                                    .is_err()
                            );
                        },
                    );
                },
            );
        }
        for broken in [
            source.replace(
                "registry_mailbox=",
                "credential_ref='google-one'\nregistry_mailbox=",
            ),
            source.replace("mb-0123456789abcdef", "../escape"),
            source.replace("mb-0123456789abcdef", "mb-wrong"),
        ] {
            assert!(super::super::google_identity::Registry::parse(&broken).is_err());
        }
        assert!(
            !root.path().join(".secrets").exists(),
            "registry source required no materialized credential file"
        );
    }

    #[cfg(unix)]
    struct Fixture {
        _root: aterm_tempfile::TempDir,
        paths: aterm_secrets::client::Paths,
        identity: Identity,
    }

    #[cfg(unix)]
    impl Fixture {
        fn new() -> Self {
            use aterm_secrets::client::{self, Paths};
            let root = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(&root.path().join("member"));
            client::create(
                &paths,
                root.path().join("fixture.vault").to_str().unwrap(),
                None,
                Some("member"),
            )
            .unwrap();
            let registry = super::super::google_identity::Registry::parse("[[identities]]\nid='one'\nemail='person@example.com'\ncredential_ref='google-one'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n").unwrap();
            let identity = registry.resolve("kimi-global", "one").unwrap().0.clone();
            Self {
                _root: root,
                paths,
                identity,
            }
        }

        fn publish(&self, name: &str, value: &str) {
            use aterm_secrets::{client, text::Audience};
            let admin = client::admin_open(&self.paths).unwrap();
            let mut doc = admin.doc.clone();
            doc.set_entry(&Audience::Everyone, name, value.as_bytes())
                .unwrap();
            client::admin_publish(&self.paths, &admin, &doc).unwrap();
        }

        fn load(&self) -> Result<Credentials, Error> {
            Credentials::load(&self.identity, &self.paths.home)
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_local_file_without_a_registered_entry_is_never_a_credential() {
        let fixture = Fixture::new();
        let path = fixture.paths.home.join(".secrets/google-one");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, VALUE).unwrap();
        assert!(matches!(fixture.load(), Err(Error::MissingEntry)));
    }

    #[cfg(unix)]
    #[test]
    fn edited_materialized_bytes_cannot_reach_the_browser() {
        let fixture = Fixture::new();
        fixture.publish("google-one", VALUE);
        let mut credentials = fixture.load().unwrap();
        std::fs::write(
            fixture.paths.home.join(".secrets/google-one"),
            VALUE.replace("fixture-only-password", "unverified-password"),
        )
        .unwrap();
        assert_eq!(
            credentials.with_google_page("https://accounts.google.com/signin", |_, _| panic!(
                "unverified bytes reached the browser"
            )),
            Err(Error::Changed)
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_google_receives_current_credentials_and_rotations_invalidate_old_reads() {
        let fixture = Fixture::new();
        fixture.publish("google-one", VALUE);
        let mut credentials = fixture.load().unwrap();
        assert_eq!(credentials.email(), "person@example.com");
        assert!(!format!("{credentials:?}").contains("fixture-only-password"));
        for url in [
            "http://accounts.google.com/",
            "https://accounts.google.com.evil.test/",
            "https://accounts.google.com@evil.test/",
            "https://accounts.google.com:444/",
            "https://www.kimi.ai/",
        ] {
            assert_eq!(
                credentials
                    .with_google_page(url, |_, _| panic!("wrong origin received credentials")),
                Err(Error::WrongOrigin)
            );
        }
        assert!(
            credentials
                .with_google_page(
                    "https://accounts.google.com/signin",
                    |password, totp| password == "fixture-only-password"
                        && totp == Some("JBSWY3DPEHPK3PXP")
                )
                .unwrap()
        );
        fixture.publish("unrelated", "inert");
        credentials.check_current().unwrap();
        fixture.publish(
            "google-one",
            &VALUE.replace("fixture-only-password", "rotated-fixture-password"),
        );
        assert_eq!(
            credentials.with_google_page("https://accounts.google.com/signin", |_, _| panic!(
                "stale credential was used"
            )),
            Err(Error::Changed)
        );
        assert!(
            fixture
                .load()
                .unwrap()
                .with_google_page("https://accounts.google.com/signin", |password, _| password
                    == "rotated-fixture-password")
                .unwrap()
        );
    }
}
