// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

#[test]
fn adopted_signing_material_is_prepared_without_changing_files_or_ownership() {
    for anchored in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("member"));
        let source = root.path().join("vault");
        test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        let cert_name = "~/.aterm/apple/devid-existing.cer";
        let key_name = "~/.aterm/apple/devid-existing.key";
        let cert = materialize::target_path(&paths.home, cert_name).unwrap();
        let key = materialize::target_path(&paths.home, key_name).unwrap();
        fsx::ensure_private_dir(cert.parent().unwrap()).unwrap();
        if !anchored {
            fsx::write_atomic(&cert, b"synthetic certificate", 0o600).unwrap();
        }
        fsx::write_atomic(&key, b"synthetic private key", 0o600).unwrap();
        let admin = admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry_with(
            &Audience::Everyone,
            cert_name,
            b"synthetic certificate",
            anchored,
            None,
        )
        .unwrap();
        doc.set_entry_with(
            &Audience::Everyone,
            key_name,
            b"synthetic private key",
            anchored,
            anchored.then_some(cert_name),
        )
        .unwrap();
        admin_publish(&paths, &admin, &doc).unwrap();
        let cfg = config(&paths).unwrap().unwrap();
        let state = load_state(&paths).unwrap();
        assert_eq!(state.manifest.files.contains_key(cert_name), anchored);
        assert!(!state.manifest.files.contains_key(key_name));
        assert_eq!(state.own.as_ref().unwrap().contains(cert_name), !anchored);
        assert!(state.own.as_ref().unwrap().contains(key_name));
        let before = (
            std::fs::read(paths.state_file()).unwrap(),
            std::fs::read(&cert).unwrap(),
            std::fs::read(&key).unwrap(),
        );
        let calls = Cell::new(0);
        let prepare = |paths: &Paths, material: &Material| {
            calls.set(calls.get() + 1);
            assert_eq!(material.cert.as_slice(), b"synthetic certificate");
            assert_eq!(material.key.as_slice(), b"synthetic private key");
            fsx::ensure_private_dir(
                crate::release_keychain::keychain_path(&paths.home)
                    .parent()
                    .unwrap(),
            )
            .unwrap();
            fsx::write_atomic(
                &crate::release_keychain::keychain_path(&paths.home),
                b"prepared",
                0o600,
            )
            .unwrap();
            fsx::write_atomic(
                &crate::release_keychain::password_path(&paths.home),
                b"prepared",
                0o600,
            )
            .unwrap();
            Ok(())
        };
        Rounds::default().step(&paths, &cfg, &prepare).unwrap();
        assert_eq!(calls.get(), 1);
        assert!(matches!(
            status_with(&paths, &cfg).unwrap(),
            SigningStatus::Ready(_)
        ));
        assert_eq!(
            before,
            (
                std::fs::read(paths.state_file()).unwrap(),
                std::fs::read(&cert).unwrap(),
                std::fs::read(&key).unwrap()
            )
        );
    }
}

#[test]
fn verified_pairs_are_prepared_and_missing_local_state_is_repaired() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let source = root.path().join("vault");
    test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let cfg = config(&paths).unwrap().unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        "~/.aterm/apple/devid-shared.cer",
        b"synthetic certificate",
    )
    .unwrap();
    doc.set_entry(
        &Audience::Everyone,
        "~/.aterm/apple/devid-shared.key",
        b"synthetic private key",
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    let calls = Cell::new(0);
    let prepare = |paths: &Paths, material: &Material| {
        calls.set(calls.get() + 1);
        assert_eq!(material.key.as_slice(), b"synthetic private key");
        let keychain = crate::release_keychain::keychain_path(&paths.home);
        std::fs::create_dir_all(keychain.parent().unwrap()).unwrap();
        fsx::write_atomic(&keychain, b"prepared", 0o600).unwrap();
        let password = crate::release_keychain::password_path(&paths.home);
        std::fs::create_dir_all(password.parent().unwrap()).unwrap();
        fsx::write_atomic(&password, b"prepared", 0o600).unwrap();
        Ok(())
    };
    let mut rounds = Rounds::default();
    let profile = paths.home.join(RELEASE_PROFILE);
    fsx::ensure_private_dir(profile.parent().unwrap()).unwrap();
    let original_profile =
        b"signing_identity_sha1 = \"0000000000000000000000000000000000000000\"\n";
    fsx::write_atomic(&profile, original_profile, 0o600).unwrap();
    assert!(matches!(
        status_with(&paths, &cfg).unwrap(),
        SigningStatus::Pending(_)
    ));
    assert!(
        rounds
            .step(&paths, &cfg, &prepare)
            .unwrap()
            .unwrap()
            .contains("proved")
    );
    assert!(matches!(
        status_with(&paths, &cfg).unwrap(),
        SigningStatus::Ready(_)
    ));
    assert!(
        effective_identity_with(&paths, &profile, true)
            .unwrap()
            .is_some()
    );
    assert!(
        effective_identity_with(&paths, &profile, false)
            .unwrap()
            .is_none()
    );
    let unrelated = paths.home.join("unrelated-release.toml");
    fsx::write_atomic(&unrelated, original_profile, 0o600).unwrap();
    assert!(
        effective_identity_with(&paths, &unrelated, true)
            .unwrap()
            .is_none()
    );
    let linked_profile = paths.home.join("linked-release.toml");
    std::os::unix::fs::symlink(&profile, &linked_profile).unwrap();
    assert!(
        effective_identity_with(&paths, &linked_profile, true)
            .unwrap()
            .is_none()
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        let keychain = crate::release_keychain::keychain_path(&paths.home);
        std::fs::set_permissions(&keychain, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            status_with(&paths, &cfg).unwrap(),
            SigningStatus::Pending(_)
        ));
        std::fs::set_permissions(&keychain, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            status_with(&paths, &cfg).unwrap(),
            SigningStatus::Ready(_)
        ));
    }
    let proof_path = paths.dir.join(PROOF);
    let proof = fsx::read_private(&proof_path, 2048).unwrap().unwrap();
    let mut record = ProofRecord::parse(proof.as_str().unwrap()).unwrap();
    record.proved = now().unwrap() - RECHECK.as_secs();
    fsx::write_atomic(&proof_path, record.text().as_bytes(), 0o600).unwrap();
    assert!(
        effective_identity_with(&paths, &profile, true)
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        status_with(&paths, &cfg).unwrap(),
        SigningStatus::Pending(_)
    ));
    record.proved = now().unwrap() + 3600;
    fsx::write_atomic(&proof_path, record.text().as_bytes(), 0o600).unwrap();
    assert!(
        effective_identity_with(&paths, &profile, true)
            .unwrap()
            .is_none()
    );
    record.proved = now().unwrap();
    record.identity = "0000000000000000000000000000000000000000".into();
    fsx::write_atomic(&proof_path, record.text().as_bytes(), 0o600).unwrap();
    assert!(
        effective_identity_with(&paths, &profile, true)
            .unwrap()
            .is_none()
    );
    fsx::write_atomic(&proof_path, proof.as_slice(), 0o600).unwrap();
    assert_eq!(std::fs::read(&profile).unwrap(), original_profile);
    assert!(rounds.step(&paths, &cfg, &prepare).unwrap().is_none());
    std::fs::remove_file(crate::release_keychain::password_path(&paths.home)).unwrap();
    assert!(
        effective_identity_with(&paths, &profile, true)
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        status_with(&paths, &cfg).unwrap(),
        SigningStatus::Pending(_)
    ));
    assert!(rounds.step(&paths, &cfg, &prepare).unwrap().is_some());
    assert_eq!(calls.get(), 2);
    fsx::write_atomic(
        &crate::release_keychain::keychain_path(&paths.home),
        b"changed",
        0o600,
    )
    .unwrap();
    assert!(matches!(
        status_with(&paths, &cfg).unwrap(),
        SigningStatus::Pending(_)
    ));
    assert!(
        effective_identity_with(&paths, &profile, true)
            .unwrap()
            .is_some()
    );
    let fail = |_: &Paths, _: &Material| Err(Error::new("synthetic repair failure"));
    assert!(rounds.step(&paths, &cfg, &fail).is_err());
    assert!(matches!(
        status_with(&paths, &cfg).unwrap(),
        SigningStatus::Pending(_)
    ));
    assert!(
        effective_identity_with(&paths, &profile, true)
            .unwrap()
            .is_none()
    );
    fsx::write_atomic(&proof_path, proof.as_slice(), 0o600).unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        "~/.aterm/apple/devid-shared.key",
        b"rotated private key",
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    assert!(
        effective_identity_with(&paths, &profile, true)
            .unwrap()
            .is_none()
    );
    // Recipient exclusion is authoritative even when an old successful receipt remains.
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine(
        "other-member",
        &crate::age::Identity::generate()
            .unwrap()
            .recipient()
            .to_bech32(),
    )
    .unwrap();
    doc.set_entry(
        &Audience::Hosts(vec!["other-member".into()]),
        "~/.aterm/apple/devid-shared.key",
        b"synthetic private key",
    )
    .unwrap();
    doc.unset_entry(&Audience::Everyone, "~/.aterm/apple/devid-shared.key")
        .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    assert!(material(&paths, &cfg).unwrap().is_none());
    assert!(
        effective_identity_with(&paths, &profile, true)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        status_with(&paths, &cfg).unwrap(),
        SigningStatus::Pending(_)
    ));
}
