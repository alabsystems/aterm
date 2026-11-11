// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::triple::AARCH64_APPLE_DARWIN;
use aterm_uds::host::upstream::CARCARA;

const TARGET: &str = "aarch64-apple-darwin";
const ARCHIVE: &str = "carcara-1.tar.zst";

struct Fixture {
    work: aterm_tempfile::TempDir,
    manifest: PathBuf,
    bytes: Vec<u8>,
    receipt: Receipt,
}

impl Fixture {
    fn new() -> Self {
        let work = aterm_tempfile::tempdir().unwrap();
        let payload = b"qualified outer archive fixture";
        let sha = atpkg::tree::sha256_bytes(payload);
        let text = format!(
            "schema = 1\nprogram = \"carcara\"\nversion = \"1.1.0\"\nbuild_number = 1\nexposes = [\"carcara\"]\n\n[[artifact]]\ntarget = \"{TARGET}\"\nkind = \"binary\"\nprotocol = \"github-release\"\nasset = \"{ARCHIVE}\"\nsha256 = \"{sha}\"\ntree_root = \"{}\"\nsize = {}\n",
            "1".repeat(64),
            payload.len()
        );
        let manifest = work.path().join("pkg-carcara-1.toml");
        std::fs::write(&manifest, &text).unwrap();
        std::fs::write(work.path().join(ARCHIVE), payload).unwrap();
        let receipt = Receipt {
            schema: 1,
            evidence: "operator-native-runtime-observation".into(),
            status: "native-pass".into(),
            native_host: Some(TARGET.into()),
            native_translation: Some("native".into()),
            control_count: CONTROLS.len(),
            controls: CONTROLS.iter().map(|s| (*s).into()).collect(),
            target: TARGET.into(),
            manifest_sha256: atpkg::tree::sha256_bytes(text.as_bytes()),
            qualified_manifest: text.clone(),
            archive: ARCHIVE.into(),
            archive_sha256: sha,
            archive_size: payload.len() as u64,
            tree_root: "1".repeat(64),
            source_cohort: super::super::witness(&CARCARA, &CARCARA.artifacts[0]),
        };
        let f = Self {
            work,
            manifest,
            bytes: text.into_bytes(),
            receipt,
        };
        f.store(&f.receipt);
        f
    }

    fn store(&self, receipt: &Receipt) {
        std::fs::write(
            sidecar(&self.manifest, ARCHIVE).unwrap(),
            aterm_json::to_vec(receipt).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn native_receipts_bind_every_observation_and_the_exact_archive() {
    let f = Fixture::new();
    let permit = require(&f.manifest, &f.bytes).unwrap().unwrap();
    assert!(permit.matches(&f.bytes));
    assert!(!permit.matches(b"different manifest"));
    for fault in [
        "pending",
        "host",
        "translation",
        "count",
        "controls",
        "target",
        "manifest",
        "archive",
        "size",
        "tree",
        "cohort",
    ] {
        let mut receipt = f.receipt.clone();
        match fault {
            "pending" => receipt.status = "pending-native".into(),
            "host" => receipt.native_host = Some("x86_64-apple-darwin".into()),
            "translation" => receipt.native_translation = Some("translated".into()),
            "count" => receipt.control_count = 0,
            "controls" => receipt.controls[0] = "honest".into(),
            "target" => receipt.target = "x86_64-apple-darwin".into(),
            "manifest" => receipt.manifest_sha256 = "0".repeat(64),
            "archive" => receipt.archive_sha256 = "0".repeat(64),
            "size" => receipt.archive_size += 1,
            "tree" => receipt.tree_root = "0".repeat(64),
            "cohort" => receipt.source_cohort = aterm_json::json!({}),
            _ => unreachable!(),
        }
        f.store(&receipt);
        assert!(require(&f.manifest, &f.bytes).is_err(), "{fault}");
    }
    f.store(&f.receipt);
    std::fs::write(
        f.work.path().join(ARCHIVE),
        b"mutated outer archive fixture!!",
    )
    .unwrap();
    assert!(require(&f.manifest, &f.bytes).is_err());
    std::fs::remove_file(sidecar(&f.manifest, ARCHIVE).unwrap()).unwrap();
    assert!(require(&f.manifest, &f.bytes).is_err());
}

#[test]
fn prefix_reuse_preserves_package_settings_and_complete_old_rows() {
    let f = Fixture::new();
    let text = std::str::from_utf8(&f.bytes).unwrap();
    let merged = format!(
        "{text}\n[[artifact]]\ntarget = \"x86_64-apple-darwin\"\nkind = \"binary\"\nasset = \"intel.tar.zst\"\n"
    );
    composition(text, merged.as_bytes(), TARGET).unwrap();
    for appended in [
        "\n[some_future_package_setting]\nvalue = true\n",
        "\n[artifact.some_future_runtime_setting]\nvalue = true\n",
    ] {
        let changed = format!("{text}{appended}");
        assert!(changed.starts_with(text));
        assert!(
            composition(text, changed.as_bytes(), TARGET).is_err(),
            "{appended}"
        );
    }
    let changed = text.replace("exposes = [\"carcara\"]", "exposes = [\"other\"]");
    assert!(composition(text, changed.as_bytes(), TARGET).is_err());
}

#[test]
fn unsigned_preparation_records_pending_and_cannot_sign_or_stage() {
    use std::sync::Arc;
    let f = Fixture::new();
    let parsed: atpkg::PkgManifest =
        aterm_toml::from_str(std::str::from_utf8(&f.bytes).unwrap()).unwrap();
    let request = BottleRequest {
        program: "carcara".into(),
        input: super::super::BottleInput::InstallTree(f.work.path().into()),
        version: "1.1.0".into(),
        build: 1,
        triple: AARCH64_APPLE_DARWIN,
        strip_components: 0,
        dep_bottles: vec![],
        donor_roots: vec![],
        merge_from: None,
        key: None,
        out: f.work.path().join("out"),
        signing: crate::api::Signing::Unsigned,
        upload: false,
    };
    write(
        &CARCARA,
        &request,
        &f.manifest,
        &f.bytes,
        &parsed.artifacts[0],
        None,
    )
    .unwrap();
    let pending: Receipt =
        aterm_json::from_slice(&std::fs::read(sidecar(&f.manifest, ARCHIVE).unwrap()).unwrap())
            .unwrap();
    assert_eq!(pending.status, "pending-native");
    assert!(pending.native_host.is_none());
    assert!(pending.native_translation.is_none());
    assert_eq!(pending.control_count, 0);
    assert!(pending.controls.is_empty());
    let host = Arc::new(crate::host::fake::FakeHost::new());
    let ctx = crate::testkit::Keys::mint("m3", &[]).ctx(f.work.path(), host.clone());
    std::fs::remove_file(&ctx.identity.key).unwrap();
    let error =
        crate::artifact::sign::sign_all(&ctx, std::slice::from_ref(&f.manifest)).unwrap_err();
    assert!(
        error.to_string().contains("native reference evidence"),
        "{error}"
    );
    assert!(!crate::artifact::sign::sig_path(&f.manifest).exists());
    assert!(
        crate::artifact::upload::publish_pack_to(
            host.as_ref(),
            &ctx.staging_slug("aterm"),
            "atpkg-carcara-1",
            &f.manifest,
            &[f.work.path().join(ARCHIVE), f.manifest.clone()],
            "fixture",
        )
        .is_err()
    );
    assert!(host.calls().is_empty());
}

#[test]
fn native_evidence_authorizes_the_normal_signer_but_not_the_unqualified_primitive() {
    use std::sync::Arc;
    let f = Fixture::new();
    let host = Arc::new(crate::host::fake::FakeHost::new());
    let keys = crate::testkit::Keys::mint("m3", &[]);
    let ctx = keys.ctx(f.work.path(), host.clone());
    keys.publish_quad(
        &host,
        &ctx.index_slug(),
        41,
        &keys.index_toml(41, &[("carcara", "aterm")], &[("carcara", 1)]),
    );
    let key = crate::identity::MachineKey::load(&ctx, &ctx.identity.key).unwrap();
    let authority = crate::artifact::sign::authority(&ctx).unwrap();
    assert!(crate::artifact::sign::sign_checked(&key, &f.bytes, &authority).is_err());
    let signatures =
        crate::artifact::sign::sign_all(&ctx, std::slice::from_ref(&f.manifest)).unwrap();
    assert_eq!(signatures.len(), 1);
    authority
        .verify_pkg(&f.bytes, &std::fs::read(&signatures[0]).unwrap())
        .unwrap();
}

#[test]
fn common_reservation_rejects_inputs_changed_after_an_early_native_check() {
    for fault in ["manifest", "archive", "receipt"] {
        let f = Fixture::new();
        require(&f.manifest, &f.bytes).unwrap().unwrap();
        match fault {
            "manifest" => {
                let changed = std::str::from_utf8(&f.bytes)
                    .unwrap()
                    .replace(&"1".repeat(64), &"2".repeat(64));
                std::fs::write(&f.manifest, changed).unwrap();
            }
            "archive" => {
                std::fs::write(
                    f.work.path().join(ARCHIVE),
                    b"changed after early admission",
                )
                .unwrap();
            }
            "receipt" => {
                let mut receipt = f.receipt.clone();
                receipt.status = "pending-native".into();
                f.store(&receipt);
            }
            _ => unreachable!(),
        }
        let host = crate::host::fake::FakeHost::new();
        let owner = crate::host::Slug::new("fixture-owner", "aterm");
        assert!(
            crate::artifact::upload::reserve_pack_version(
                &host,
                &owner,
                "atpkg-carcara-1",
                &f.manifest,
                &[f.work.path().join(ARCHIVE), f.manifest.clone()],
                "fixture"
            )
            .is_err(),
            "{fault}"
        );
        assert!(host.calls().is_empty(), "{fault}");
    }
}

#[test]
fn admitted_upload_owns_the_same_manifest_and_archive_after_originals_change() {
    let f = Fixture::new();
    let original_archive = std::fs::read(f.work.path().join(ARCHIVE)).unwrap();
    let host = crate::host::fake::FakeHost::new();
    let owner = crate::host::Slug::new("fixture-owner", "aterm");
    let reserved = crate::artifact::upload::reserve_pack_version(
        &host,
        &owner,
        "atpkg-carcara-1",
        &f.manifest,
        &[f.work.path().join(ARCHIVE), f.manifest.clone()],
        "fixture",
    )
    .unwrap();
    std::fs::write(&f.manifest, b"mutated original manifest").unwrap();
    std::fs::write(f.work.path().join(ARCHIVE), b"mutated original archive").unwrap();
    std::fs::remove_file(sidecar(&f.manifest, ARCHIVE).unwrap()).unwrap();
    for path in reserved.files() {
        assert_ne!(path.parent(), Some(f.work.path()));
        if path.file_name().unwrap() == ARCHIVE {
            assert_eq!(std::fs::read(path).unwrap(), original_archive);
        } else if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".qualification.json")
        {
            let receipt: Receipt = aterm_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(receipt.manifest_sha256, atpkg::tree::sha256_bytes(&f.bytes));
        } else {
            assert_eq!(std::fs::read(path).unwrap(), f.bytes);
        }
    }
}

#[test]
fn a_reference_mirror_carries_native_evidence_and_refuses_its_absence() {
    use std::sync::Arc;
    let f = Fixture::new();
    let host = Arc::new(crate::host::fake::FakeHost::new());
    let keys = crate::testkit::Keys::mint("m3", &[]);
    let ctx = keys.ctx(f.work.path(), host.clone());
    let index = keys.index_toml(41, &[("carcara", "aterm")], &[("carcara", 1)]);
    keys.publish_quad(&host, &ctx.index_slug(), 41, &index);
    crate::artifact::sign::sign_all(&ctx, std::slice::from_ref(&f.manifest)).unwrap();
    let quad_dir = f.work.path().join("quad");
    keys.write_quad(&quad_dir, &index);
    let quad = crate::quad::verify_dir(&ctx, &quad_dir).unwrap();
    let mut names = vec![
        "pkg-carcara-1.toml".into(),
        "pkg-carcara-1.toml.sig".into(),
        ARCHIVE.into(),
    ];
    let missing =
        crate::channel::mirror::check_staged(&ctx, &quad, f.work.path(), &names, "carcara", 1);
    assert!(missing.is_err());
    let receipt = receipt_names(&f.bytes).unwrap().pop().unwrap();
    names.push(receipt.clone());
    let (expected, _) =
        crate::channel::mirror::check_staged(&ctx, &quad, f.work.path(), &names, "carcara", 1)
            .unwrap();
    assert!(expected.contains(&receipt));
    let staging = ctx.staging_slug("aterm");
    for name in &names {
        host.put_asset(
            &staging,
            "atpkg-carcara-1",
            name,
            std::fs::read(f.work.path().join(name)).unwrap(),
        );
    }
    let mut log = crate::channel::Log::capture("mirror");
    crate::channel::mirror::mirror(
        &ctx,
        &quad,
        &crate::channel::mirror::MirrorOptions { dry_run: false },
        &mut log,
    )
    .unwrap();
    let public = ctx.public_slug("aterm");
    let public_receipt = host.asset(&public, "atpkg-carcara-1", &receipt).unwrap();
    let public_receipt: Receipt = aterm_json::from_slice(&public_receipt).unwrap();
    assert_eq!(public_receipt.status, "native-pass");
    assert_eq!(public_receipt.archive_sha256, f.receipt.archive_sha256);
}
