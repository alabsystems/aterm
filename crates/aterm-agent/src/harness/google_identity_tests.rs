// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    assert!(Registry::parse(&with_mail.replace("recovery@example.com", "not an email")).is_err());
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
        Registry::parse(&REGISTRY.replace("Person.Name+ai@gmail.com", "other@gmail.com")).unwrap();
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
