// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native observations live outside the payload and are required by reference signing/staging.
//! They are operator/producer evidence, not cryptographic proof of execution.

use std::path::{Path, PathBuf};

use aterm_uds::host::upstream::ReferenceTool;

use crate::artifact::bottle::BottleRequest;
use crate::error::{Error, Result};
use crate::programs::{BottleOrigin, Shape};
use crate::triple::Triple;

const CONTROLS: [&str; 7] = [
    "exact-version",
    "honest",
    "false-resolution",
    "wrong-problem",
    "truncated",
    "invalid-utf8",
    "missing",
];

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Receipt {
    schema: u32,
    evidence: String,
    status: String,
    native_host: Option<String>,
    native_translation: Option<String>,
    control_count: usize,
    controls: Vec<String>,
    target: String,
    manifest_sha256: String,
    qualified_manifest: String,
    archive: String,
    archive_sha256: String,
    archive_size: u64,
    tree_root: String,
    source_cohort: aterm_json::Value,
}

/// An operation-local witness that every source-derived row has matching native evidence.
pub(crate) struct Permit {
    manifest_sha256: String,
}

impl Permit {
    pub(crate) fn matches(&self, bytes: &[u8]) -> bool {
        atpkg::tree::sha256_bytes(bytes) == self.manifest_sha256
    }
}

fn parse(bytes: &[u8]) -> Result<aterm_toml::Value> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| Error::refused(format!("reference manifest UTF-8: {e}")))?;
    aterm_toml::from_str(text).map_err(|e| Error::refused(format!("reference manifest: {e}")))
}

fn recipe(doc: &aterm_toml::Value) -> Option<&'static ReferenceTool> {
    let program = doc.get("program")?.as_str()?;
    let def = crate::programs::get(program)?;
    match def.shape {
        Shape::Bottle(crate::programs::BottleDef {
            origin: BottleOrigin::Reference(recipe),
            ..
        }) => Some(recipe),
        _ => None,
    }
}

pub(crate) fn is_reference(bytes: &[u8]) -> Result<bool> {
    Ok(recipe(&parse(bytes)?).is_some())
}

/// Additional, validated evidence files carried by a source-derived release.
pub(crate) fn receipt_names(bytes: &[u8]) -> Result<Vec<String>> {
    if !is_reference(bytes)? {
        return Ok(vec![]);
    }
    let parsed: atpkg::PkgManifest = aterm_toml::from_str(
        std::str::from_utf8(bytes)
            .map_err(|e| Error::refused(format!("reference manifest UTF-8: {e}")))?,
    )
    .map_err(|e| Error::refused(format!("reference manifest: {e}")))?;
    parsed
        .artifacts
        .iter()
        .map(|a| {
            sidecar(Path::new("pkg.toml"), &a.asset)?
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
                .ok_or_else(|| Error::refused("reference receipt has no file name"))
        })
        .collect()
}

fn sidecar(manifest: &Path, asset: &str) -> Result<PathBuf> {
    if Path::new(asset).components().count() != 1
        || !matches!(
            Path::new(asset).components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(Error::refused("reference archive name is not a file name"));
    }
    Ok(manifest
        .parent()
        .ok_or_else(|| Error::refused("reference manifest has no directory"))?
        .join(format!("{asset}.qualification.json")))
}

pub(crate) fn write(
    recipe: &ReferenceTool,
    request: &BottleRequest,
    manifest: &Path,
    bytes: &[u8],
    artifact: &atpkg::Artifact,
    native_controls: Option<usize>,
) -> Result<()> {
    if native_controls.is_some_and(|n| n != CONTROLS.len()) {
        return Err(Error::refused(
            "reference native control count differs from its receipt contract",
        ));
    }
    let receipt = Receipt {
        schema: 1,
        evidence: "operator-native-runtime-observation".into(),
        status: if native_controls.is_some() {
            "native-pass"
        } else {
            "pending-native"
        }
        .into(),
        native_host: native_controls
            .and_then(|_| super::native_host())
            .map(|t| t.as_str().to_owned()),
        native_translation: native_controls.map(|_| "native".into()),
        control_count: native_controls.unwrap_or(0),
        controls: if native_controls.is_some() {
            CONTROLS.iter().map(|s| (*s).to_owned()).collect()
        } else {
            vec![]
        },
        target: artifact.target.clone(),
        manifest_sha256: atpkg::tree::sha256_bytes(bytes),
        qualified_manifest: std::str::from_utf8(bytes)
            .map_err(|e| Error::refused(format!("reference manifest UTF-8: {e}")))?
            .into(),
        archive: artifact.asset.clone(),
        archive_sha256: artifact.sha256.clone(),
        archive_size: artifact.size,
        tree_root: artifact.tree_root.clone(),
        source_cohort: super::witness(recipe, super::artifact(recipe, request.triple)?),
    };
    let path = sidecar(manifest, &artifact.asset)?;
    crate::lane::report::write_atomic_bytes(
        &path,
        &aterm_json::to_vec(&receipt)
            .map_err(|e| Error::refused(format!("reference receipt: {e}")))?,
    )?;
    eprintln!(
        "atpkg-publish: reference qualification {} — {}",
        receipt.status,
        path.display()
    );
    Ok(())
}

fn row(doc: &aterm_toml::Value, target: &str) -> Result<aterm_toml::Value> {
    let rows = doc
        .get("artifact")
        .and_then(aterm_toml::Value::as_array)
        .ok_or_else(|| Error::refused("reference manifest has no artifact rows"))?;
    let mut rows = rows
        .iter()
        .filter(|r| r.get("target").and_then(aterm_toml::Value::as_str) == Some(target));
    let found = rows
        .next()
        .ok_or_else(|| Error::refused("reference receipt omits its target"))?;
    if rows.next().is_some() {
        return Err(Error::refused("reference manifest repeats a target"));
    }
    Ok(found.clone())
}

fn globals(mut doc: aterm_toml::Value) -> Result<aterm_toml::Value> {
    doc.as_table_mut()
        .ok_or_else(|| Error::refused("reference manifest is not a table"))?
        .remove("artifact");
    Ok(doc)
}

fn composition(prior: &str, bytes: &[u8], target: &str) -> Result<()> {
    if !bytes.starts_with(prior.as_bytes()) {
        return Err(Error::refused(
            "reference manifest is not the qualified byte prefix",
        ));
    }
    let prior = parse(prior.as_bytes())?;
    let current = parse(bytes)?;
    if globals(prior.clone())? != globals(current.clone())?
        || row(&prior, target)? != row(&current, target)?
    {
        return Err(Error::refused(
            "reference composition changed qualified package settings or an artifact row",
        ));
    }
    Ok(())
}

/// Require native evidence for every reference row, including unchanged rows carried by a merge.
/// The checked exact bytes are retained by the signer, rather than fetched/read again.
pub(crate) fn require(manifest: &Path, bytes: &[u8]) -> Result<Option<Permit>> {
    let doc = parse(bytes)?;
    let Some(recipe) = recipe(&doc) else {
        return Ok(None);
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|e| Error::refused(format!("reference manifest UTF-8: {e}")))?;
    let parsed: atpkg::PkgManifest = aterm_toml::from_str(text)
        .map_err(|e| Error::refused(format!("reference manifest: {e}")))?;
    if parsed.version != recipe.version
        || parsed.exposes != [recipe.name]
        || !parsed.requires.is_empty()
        || !parsed.shim_env.is_empty()
        || parsed.artifacts.is_empty()
    {
        return Err(Error::refused(
            "reference package settings differ from the source recipe",
        ));
    }
    for artifact in &parsed.artifacts {
        let path = sidecar(manifest, &artifact.asset)?;
        let data = crate::identity::read_regular_bytes(&path, crate::identity::MAX_META_BYTES)
            .map_err(|e| {
                Error::io(
                    format!("read native reference receipt {}", path.display()),
                    &e,
                )
            })?;
        let receipt: Receipt = aterm_json::from_slice(&data)
            .map_err(|e| Error::refused(format!("native reference receipt: {e}")))?;
        let target = Triple::parse(&artifact.target)?;
        if receipt.schema != 1
            || receipt.evidence != "operator-native-runtime-observation"
            || receipt.status != "native-pass"
            || receipt.native_host.as_deref() != Some(artifact.target.as_str())
            || receipt.native_translation.as_deref() != Some("native")
            || receipt.control_count != receipt.controls.len()
            || receipt.control_count != CONTROLS.len()
            || !CONTROLS.iter().all(|name| {
                receipt
                    .controls
                    .iter()
                    .filter(|got| got.as_str() == *name)
                    .count()
                    == 1
            })
            || receipt.target != artifact.target
            || receipt.archive != artifact.asset
            || receipt.archive_sha256 != artifact.sha256
            || receipt.archive_size != artifact.size
            || receipt.tree_root != artifact.tree_root
            || receipt.source_cohort != super::witness(recipe, super::artifact(recipe, target)?)
            || atpkg::tree::sha256_bytes(receipt.qualified_manifest.as_bytes())
                != receipt.manifest_sha256
        {
            return Err(Error::refused(format!(
                "{} lacks complete matching native reference evidence",
                artifact.target
            )));
        }
        composition(&receipt.qualified_manifest, bytes, &artifact.target)?;
        let archive = manifest
            .parent()
            .ok_or_else(|| Error::refused("reference manifest has no directory"))?
            .join(&artifact.asset);
        let metadata = std::fs::metadata(&archive)
            .map_err(|e| Error::io("stat qualified reference archive", &e))?;
        if !metadata.is_file()
            || metadata.len() != artifact.size
            || atpkg::sha256_file(&archive)
                .map_err(|e| Error::io("hash qualified reference archive", &e))?
                != artifact.sha256
        {
            return Err(Error::refused(
                "reference archive changed after native qualification",
            ));
        }
    }
    Ok(Some(Permit {
        manifest_sha256: atpkg::tree::sha256_bytes(bytes),
    }))
}

fn copy_snapshot(source: &Path, destination: &Path, maximum: u64) -> Result<()> {
    use std::io::Read as _;
    let (file, metadata) = crate::identity::open_regular_nonblock(source)
        .map_err(|e| Error::io("open source reference snapshot", &e))?;
    if metadata.len() > maximum {
        return Err(Error::refused(
            "source reference snapshot exceeds its file bound",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut copied = options
        .open(destination)
        .map_err(|e| Error::io("create source reference snapshot", &e))?;
    let length =
        std::io::copy(
            &mut file.take(maximum.checked_add(1).ok_or_else(|| {
                Error::refused("source snapshot bound cannot admit a sentinel byte")
            })?),
            &mut copied,
        )
        .map_err(|e| Error::io("copy source reference snapshot", &e))?;
    if length != metadata.len() || length > maximum {
        return Err(Error::refused(
            "source reference input changed while it was copied",
        ));
    }
    Ok(())
}

/// Freeze the files the common reservation/upload boundary will actually use.
/// Only source-derived references need the additional native evidence policy.
pub(crate) fn snapshot(
    original_manifest: &Path,
    prepared_manifest: &Path,
    bytes: &[u8],
    files: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    if !is_reference(bytes)? {
        return Ok(files.to_vec());
    }
    let directory = prepared_manifest
        .parent()
        .ok_or_else(|| Error::refused("reference snapshot has no directory"))?;
    let mut frozen = Vec::new();
    for file in files {
        if file == prepared_manifest {
            frozen.push(file.clone());
            continue;
        }
        let name = file
            .file_name()
            .ok_or_else(|| Error::refused("reference asset has no file name"))?;
        let destination = directory.join(name);
        copy_snapshot(
            file,
            &destination,
            aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND,
        )?;
        frozen.push(destination);
    }
    let parsed: atpkg::PkgManifest = aterm_toml::from_str(
        std::str::from_utf8(bytes)
            .map_err(|e| Error::refused(format!("reference manifest UTF-8: {e}")))?,
    )
    .map_err(|e| Error::refused(format!("reference manifest: {e}")))?;
    for artifact in &parsed.artifacts {
        let destination = sidecar(prepared_manifest, &artifact.asset)?;
        if !destination.exists() {
            copy_snapshot(
                &sidecar(original_manifest, &artifact.asset)?,
                &destination,
                crate::identity::MAX_META_BYTES,
            )?;
        }
    }
    let permit = require(prepared_manifest, bytes)?
        .ok_or_else(|| Error::refused("reference snapshot lost its source identity"))?;
    if !permit.matches(bytes) {
        return Err(Error::refused("reference snapshot lost its native permit"));
    }
    // Publish only the typed evidence fields, without unrelated operator log data.
    // Keep the ready manifest last, after archives, signatures and evidence.
    for artifact in &parsed.artifacts {
        let path = sidecar(prepared_manifest, &artifact.asset)?;
        let data = crate::identity::read_regular_bytes(&path, crate::identity::MAX_META_BYTES)
            .map_err(|e| Error::io("read admitted native receipt", &e))?;
        let receipt: Receipt = aterm_json::from_slice(&data)
            .map_err(|e| Error::refused(format!("native reference receipt: {e}")))?;
        crate::lane::report::write_atomic_bytes(
            &path,
            &aterm_json::to_vec(&receipt)
                .map_err(|e| Error::refused(format!("native reference receipt: {e}")))?,
        )?;
        if !frozen.contains(&path) {
            let before_manifest = frozen
                .iter()
                .position(|p| p == prepared_manifest)
                .ok_or_else(|| Error::refused("reference snapshot lost its ready manifest"))?;
            frozen.insert(before_manifest, path);
        }
    }
    Ok(frozen)
}

#[cfg(test)]
mod tests {
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
                let receipt: Receipt =
                    aterm_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
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
}
