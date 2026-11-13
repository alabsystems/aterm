// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};
fn fixture(root: &Path) -> (SignedRelease, String, Vec<u8>) {
    let master = Ed25519KeyPair::from_seed_unchecked(&[21; 32]).unwrap();
    let signer = Ed25519KeyPair::from_seed_unchecked(&[22; 32]).unwrap();
    let key =
        |pair: &Ed25519KeyPair| aterm_codec::base64::encode(pair.public_key().as_ref()).unwrap();
    let roster = format!("schema=1\nroster_seq=3\nvalid_until='2099-01-01T00:00:00Z'\n[[machine]]\nid='m1'\npubkey='{}'\n", key(&signer)).into_bytes();
    let executable = if root.join("Contents/MacOS/aterm").is_file() {
        PathBuf::from("Contents/MacOS/aterm")
    } else {
        PathBuf::from("aterm")
    };
    let mut declaration = ServiceDeclaration {
        schema: 1,
        platform: "aarch64-apple-darwin".into(),
        image_sha256: image_digest(root).unwrap(),
        executable_sha256: sha256_file(&root.join(&executable)).unwrap(),
        executable,
        qualification_sha256: String::new(),
        protocol: ProtocolRange { min: 1, max: 2 },
    };
    let report = aterm_json::to_vec(&Qualification {
        schema: 1,
        status: "passed".into(),
        scope: "release-gate-and-signed-image-protocol".into(),
        build: 149,
        source_revision: "a".repeat(40),
        verified_source_revision: "d".repeat(40),
        verified_input_sha256: "e".repeat(64),
        release_check_evidence: "release gate accepted".into(),
        image_sha256: declaration.image_sha256.clone(),
        executable_sha256: declaration.executable_sha256.clone(),
        protocol: declaration.protocol.clone(),
        protocol_probe_passed: true,
        native_signature_verified: true,
        live_service_health_tested: false,
    })
    .unwrap();
    declaration.qualification_sha256 = digest(&report);
    let manifest = format!(
        "schema=1\nversion='0.149.0'\nbuild_number=149\ncommit='{}'\ndmg='aterm.dmg'\nsha256='{}'\nmachine_id='m1'\nroster_seq=3\n{}",
        "a".repeat(40),
        "b".repeat(64),
        aterm_toml::to_string(&Extension {
            service: declaration
        })
        .unwrap()
    );
    (
        SignedRelease {
            manifest_signature: signer.sign(manifest.as_bytes()).as_ref().to_vec(),
            manifest,
            roster_signature: master.sign(&roster).as_ref().to_vec(),
            roster,
        },
        key(&master),
        report,
    )
}
#[test]
fn authenticated_store_rechecks_all_bytes_and_authority() {
    let tmp = crate::scratch_dir("service-artifact");
    let input = tmp.path().join("input");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(input.join("aterm"), b"signed executable").unwrap();
    std::fs::write(input.join("resource"), b"signed resource").unwrap();
    let (signed, key, report) = fixture(&input);
    let replacement = Ed25519KeyPair::from_seed_unchecked(&[23; 32]).unwrap();
    let replacement_key = aterm_codec::base64::encode(replacement.public_key().as_ref()).unwrap();
    assert!(authenticate(&signed, &report, 0, 3, &[&replacement_key, &key]).is_err());
    let root = tmp.path().join("store");
    ensure_private_dir(&root).unwrap();
    let store = Store { root };
    let receipt = store
        .materialize_with(&signed, &input, &report, 0, 3, &[&key])
        .unwrap();
    assert_eq!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
            .unwrap(),
        receipt
    );
    assert!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 4, &[&key])
            .is_err()
    );
    assert!(
        store
            .verify_with(&receipt.service.image_sha256, 9_000_000_000, 3, &[&key])
            .is_err()
    );
    assert!(
        store
            .materialize_with(&signed, &input, b"forged report", 0, 3, &[&key])
            .is_err()
    );
    let mut forged = signed.clone();
    forged.manifest.push_str("\n# forged");
    assert!(
        store
            .materialize_with(&forged, &input, &report, 0, 3, &[&key])
            .is_err()
    );
    std::fs::write(
        store
            .root
            .join(&receipt.service.image_sha256)
            .join(IMAGE_NAME)
            .join("resource"),
        b"modified",
    )
    .unwrap();
    assert!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
            .is_err()
    );
}
#[test]
fn shipped_bundle_aliases_survive_materialization_and_bind_their_targets() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let tmp = crate::scratch_dir("service-artifact");
    let input = tmp.path().join("aterm.app");
    let binaries = input.join("Contents/MacOS");
    let resources = input.join("Contents/Resources");
    std::fs::create_dir_all(&binaries).unwrap();
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::write(binaries.join("aterm"), b"bundle executable fixture").unwrap();
    std::fs::set_permissions(
        binaries.join("aterm"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(binaries.join("alternate"), b"bundle executable fixture").unwrap();
    std::fs::write(resources.join("resource.dat"), b"bundle resources").unwrap();
    std::fs::write(
        input.join("Contents/Info.plist"),
        b"<plist><dict><key>CFBundleExecutable</key><string>aterm</string></dict></plist>",
    )
    .unwrap();
    for alias in aterm_types::argv0::ALIASES {
        symlink("aterm", binaries.join(alias)).unwrap();
    }

    let (signed, key, report) = fixture(&input);
    let root = tmp.path().join("store");
    ensure_private_dir(&root).unwrap();
    let store = Store { root };
    let receipt = store
        .materialize_with(&signed, &input, &report, 0, 3, &[&key])
        .unwrap();
    let executable = store.executable(&receipt).unwrap();
    let stored_binaries = executable.parent().unwrap();
    for alias in aterm_types::argv0::ALIASES {
        let path = stored_binaries.join(alias);
        assert_eq!(std::fs::read_link(&path).unwrap(), Path::new("aterm"));
        assert_eq!(
            path.canonicalize().unwrap(),
            executable.canonicalize().unwrap()
        );
    }
    let image = store
        .root
        .join(&receipt.service.image_sha256)
        .join(IMAGE_NAME);
    assert_eq!(image_digest(&image).unwrap(), receipt.service.image_sha256);
    assert!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
            .is_ok()
    );

    let changed = stored_binaries.join(aterm_types::argv0::ALIASES[0]);
    std::fs::remove_file(&changed).unwrap();
    symlink("alternate", &changed).unwrap();
    assert_ne!(image_digest(&image).unwrap(), receipt.service.image_sha256);
    assert!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
            .is_err()
    );

    std::fs::write(image.parent().unwrap().join("outside"), b"outside image").unwrap();
    std::fs::remove_file(&changed).unwrap();
    symlink("../../../outside", &changed).unwrap();
    assert!(image_digest(&image).is_err());
    assert!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
            .is_err()
    );
}
#[test]
fn absent_declaration_and_revoked_signer_refuse() {
    let tmp = crate::scratch_dir("service-artifact");
    std::fs::write(tmp.path().join("aterm"), b"exe").unwrap();
    let (mut signed, key, report) = fixture(tmp.path());
    let signer = Ed25519KeyPair::from_seed_unchecked(&[22; 32]).unwrap();
    signed.manifest = signed.manifest.split("[service]").next().unwrap().into();
    signed.manifest_signature = signer.sign(signed.manifest.as_bytes()).as_ref().to_vec();
    assert!(authenticate(&signed, &report, 0, 3, &[&key]).is_err());
    let (mut signed, _, report) = fixture(tmp.path());
    signed.roster = String::from_utf8(signed.roster)
        .unwrap()
        .replace("[[machine]]", "revoked=['m1']\n[[machine]]")
        .into_bytes();
    let master = Ed25519KeyPair::from_seed_unchecked(&[21; 32]).unwrap();
    signed.roster_signature = master.sign(&signed.roster).as_ref().to_vec();
    assert!(authenticate(&signed, &report, 0, 3, &[&key]).is_err());
}
#[test]
fn new_roster_reauthorizes_old_release_and_revocation_removes_authority() {
    let tmp = crate::scratch_dir("service-artifact");
    std::fs::write(tmp.path().join("aterm"), b"exe").unwrap();
    let (mut signed, key, report) = fixture(tmp.path());
    let master = Ed25519KeyPair::from_seed_unchecked(&[21; 32]).unwrap();
    signed.roster = String::from_utf8(signed.roster)
        .unwrap()
        .replace("roster_seq=3", "roster_seq=4")
        .into_bytes();
    signed.roster_signature = master.sign(&signed.roster).as_ref().to_vec();
    assert!(authenticate(&signed, &report, 0, 4, &[&key]).is_ok());
    signed.roster = String::from_utf8(signed.roster)
        .unwrap()
        .replace("[[machine]]", "revoked=['m1']\n[[machine]]")
        .into_bytes();
    signed.roster_signature = master.sign(&signed.roster).as_ref().to_vec();
    assert!(authenticate(&signed, &report, 0, 4, &[&key]).is_err());
}
#[test]
fn restaging_refreshes_authority_atomically_without_replacing_the_image() {
    use std::os::unix::fs::MetadataExt;
    let tmp = crate::scratch_dir("service-artifact");
    let input = tmp.path().join("input");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(input.join("aterm"), b"image bytes").unwrap();
    let (signed, key, report) = fixture(&input);
    let root = tmp.path().join("store");
    ensure_private_dir(&root).unwrap();
    let store = Store { root };
    let receipt = store
        .materialize_with(&signed, &input, &report, 0, 3, &[&key])
        .unwrap();
    let executable = store.executable(&receipt).unwrap();
    let inode = std::fs::metadata(&executable).unwrap().ino();
    assert!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 4, &[&key])
            .is_err()
    );
    let mut newer = signed.clone();
    newer.roster = String::from_utf8(newer.roster)
        .unwrap()
        .replace("roster_seq=3", "roster_seq=4")
        .into_bytes();
    newer.roster_signature = Ed25519KeyPair::from_seed_unchecked(&[21; 32])
        .unwrap()
        .sign(&newer.roster)
        .as_ref()
        .to_vec();
    #[cfg(target_os = "macos")]
    {
        crate::manifest::fail_next_durable_write();
        assert!(
            store
                .materialize_with(&newer, &input, &report, 0, 4, &[&key])
                .is_err()
        );
        assert!(
            store
                .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
                .is_ok()
        );
    }
    assert_eq!(
        store
            .materialize_with(&newer, &input, &report, 0, 4, &[&key])
            .unwrap(),
        receipt
    );
    assert_eq!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 4, &[&key])
            .unwrap(),
        receipt
    );
    assert_eq!(std::fs::metadata(&executable).unwrap().ino(), inode);
}
#[test]
fn even_authentically_signed_failed_or_different_qualifications_refuse() {
    let tmp = crate::scratch_dir("service-artifact");
    std::fs::write(tmp.path().join("aterm"), b"exe").unwrap();
    let (signed, key, report) = fixture(tmp.path());
    let signer = Ed25519KeyPair::from_seed_unchecked(&[22; 32]).unwrap();
    for case in 0..5 {
        let mut failed: Qualification = aterm_json::from_slice(&report).unwrap();
        match case {
            0 => failed.status = "failed".into(),
            1 => failed.schema = 2,
            2 => failed.image_sha256 = "f".repeat(64),
            3 => failed.release_check_evidence.clear(),
            _ => failed.native_signature_verified = false,
        }
        let failed_report = aterm_json::to_vec(&failed).unwrap();
        let mut candidate = signed.clone();
        candidate.manifest = candidate
            .manifest
            .replace(&digest(&report), &digest(&failed_report));
        candidate.manifest_signature = signer.sign(candidate.manifest.as_bytes()).as_ref().to_vec();
        assert!(
            authenticate(&candidate, &failed_report, 0, 3, &[&key]).is_err(),
            "case {case}"
        );
    }
}
#[test]
fn linked_escape_and_protocol_gaps_refuse() {
    let tmp = crate::scratch_dir("service-artifact");
    std::os::unix::fs::symlink("/etc/passwd", tmp.path().join("escape")).unwrap();
    assert!(image_digest(tmp.path()).is_err());
    let p = ProtocolRange { min: 2, max: 4 };
    assert!(p.supports(&[2, 3, 4]));
    assert!(!p.supports(&[1, 4]));
    assert!(!p.supports(&[2, 5]));
    assert!(!ProtocolRange { min: 0, max: 4 }.supports(&[]));
    for root in [
        "relative",
        "/tmp/service-store",
        "/home/test/target/service-store",
        "/home/test/Library/Caches/service-store",
    ] {
        assert!(Store::new(root.into()).is_err());
    }
}
#[test]
fn reentering_symlinks_symlinked_executables_and_open_or_unknown_receipts_refuse() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let tmp = crate::scratch_dir("service-artifact");
    let reenter = tmp.path().join("reenter");
    std::fs::create_dir(&reenter).unwrap();
    std::fs::write(reenter.join("aterm"), b"exe").unwrap();
    // Resolves back inside `reenter`, yet steps above root via `..`.
    symlink("../reenter/aterm", reenter.join("bounce")).unwrap();
    assert!(image_digest(&reenter).is_err());

    let input = tmp.path().join("input");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(input.join("aterm"), b"exe").unwrap();
    symlink("aterm", input.join("alias")).unwrap();
    let (mut signed, key, report) = fixture(&input);
    let signer = Ed25519KeyPair::from_seed_unchecked(&[22; 32]).unwrap();
    signed.manifest = signed
        .manifest
        .replace("executable = \"aterm\"", "executable = \"alias\"");
    signed.manifest_signature = signer.sign(signed.manifest.as_bytes()).as_ref().to_vec();
    let root = tmp.path().join("store");
    ensure_private_dir(&root).unwrap();
    let store = Store { root };
    assert!(
        store
            .materialize_with(&signed, &input, &report, 0, 3, &[&key])
            .is_err()
    );

    let (signed, key, report) = fixture(&input);
    let receipt = store
        .materialize_with(&signed, &input, &report, 0, 3, &[&key])
        .unwrap();
    let receipt_path = store
        .root
        .join(&receipt.service.image_sha256)
        .join("receipt.json");
    let saved = std::fs::read(&receipt_path).unwrap();
    std::fs::set_permissions(&receipt_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
            .is_err()
    );
    let mut with_extra: aterm_json::Value = aterm_json::from_slice(&saved).unwrap();
    with_extra
        .as_object_mut()
        .unwrap()
        .insert("unexpected".into(), aterm_json::json!(true));
    aterm_uds::private_file::write_private(
        receipt_path.parent().unwrap(),
        "receipt.json",
        &aterm_json::to_vec(&with_extra).unwrap(),
    )
    .unwrap();
    assert!(
        store
            .verify_with(&receipt.service.image_sha256, 0, 3, &[&key])
            .is_err()
    );
}

/// NEGATIVE CONTROL: symlinked store ancestors are refused before directory creation, and
/// dangling symlinks or regular files at a content-addressed store slot refuse materialization.
#[test]
fn symlinked_store_ancestors_and_non_directory_slots_refuse() {
    use std::os::unix::fs::symlink;

    let tmp = crate::scratch_dir("service-artifact");
    let canonical = tmp.path().canonicalize().unwrap();
    let real_parent = canonical.join("real");
    std::fs::create_dir(&real_parent).unwrap();
    assert!(check_no_symlink_ancestors(&real_parent.join("service-images")).is_ok());

    let linked_parent = canonical.join("linked");
    symlink(&real_parent, &linked_parent).unwrap();
    assert!(check_no_symlink_ancestors(&linked_parent.join("service-images")).is_err());

    let input = canonical.join("input");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(input.join("aterm"), b"exe").unwrap();
    let (signed, key, report) = fixture(&input);
    let expected_id = image_digest(&input).unwrap();

    let root = canonical.join("store");
    ensure_private_dir(&root).unwrap();
    let store = Store { root };

    let slot = store.root.join(&expected_id);
    symlink(canonical.join("nonexistent"), &slot).unwrap();
    let err = store
        .materialize_with(&signed, &input, &report, 0, 3, &[&key])
        .expect_err("dangling symlink at store slot must refuse");
    assert!(err.contains("real directory"), "{err}");
    std::fs::remove_file(&slot).unwrap();

    std::fs::write(&slot, b"not a directory").unwrap();
    let err = store
        .materialize_with(&signed, &input, &report, 0, 3, &[&key])
        .expect_err("regular file at store slot must refuse");
    assert!(err.contains("real directory"), "{err}");
}
