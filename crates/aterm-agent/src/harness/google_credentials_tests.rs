// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
                                .with_google_page("https://accounts.google.com/", |_, _| panic!(
                                    "changed credential reached browser"
                                ))
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
            credentials.with_google_page(url, |_, _| panic!("wrong origin received credentials")),
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
