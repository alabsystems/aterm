// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native observations live outside the payload and are required by reference signing/staging.
//! They are operator/producer evidence, not cryptographic proof of execution.

use std::path::{Path, PathBuf};

use aterm_uds::host::upstream::ReferenceTool;

use crate::artifact::bottle::{BottleRequest, agit};
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

#[derive(Clone, Copy)]
enum Product {
    Reference(&'static ReferenceTool),
    Agit,
}

impl Product {
    fn controls(self) -> &'static [&'static str] {
        match self {
            Self::Reference(_) => &CONTROLS,
            Self::Agit => agit::CONTROLS,
        }
    }
}

fn product(doc: &aterm_toml::Value) -> Option<Product> {
    let program = doc.get("program")?.as_str()?;
    let Shape::Bottle(bottle) = crate::programs::get(program)?.shape else {
        return None;
    };
    match bottle.origin {
        BottleOrigin::Reference(recipe) => Some(Product::Reference(recipe)),
        BottleOrigin::Agit => Some(Product::Agit),
        _ => None,
    }
}

pub(crate) fn requires_native_receipts(bytes: &[u8]) -> Result<bool> {
    Ok(product(&parse(bytes)?).is_some())
}

/// Additional, validated evidence files carried by a source-derived release.
pub(crate) fn receipt_names(bytes: &[u8]) -> Result<Vec<String>> {
    if !requires_native_receipts(bytes)? {
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
    let cohort = super::witness(recipe, super::artifact(recipe, request.triple)?);
    store(
        manifest,
        bytes,
        artifact,
        cohort,
        &CONTROLS,
        native_controls,
    )
}

pub(in crate::artifact::bottle) fn write_agit(
    origin: &agit::BuildOrigin,
    manifest: &Path,
    bytes: &[u8],
    artifact: &atpkg::Artifact,
) -> Result<()> {
    let cohort = aterm_json::to_value(origin)
        .map_err(|e| Error::refused(format!("agit source record: {e}")))?;
    store(
        manifest,
        bytes,
        artifact,
        cohort,
        agit::CONTROLS,
        Some(agit::CONTROLS.len()),
    )
}

fn store(
    manifest: &Path,
    bytes: &[u8],
    artifact: &atpkg::Artifact,
    source_cohort: aterm_json::Value,
    controls: &[&str],
    native_controls: Option<usize>,
) -> Result<()> {
    if native_controls.is_some_and(|n| n != controls.len()) {
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
            controls.iter().map(|s| (*s).to_owned()).collect()
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
        source_cohort,
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
    let Some(product) = product(&doc) else {
        return Ok(None);
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|e| Error::refused(format!("reference manifest UTF-8: {e}")))?;
    let parsed: atpkg::PkgManifest = aterm_toml::from_str(text)
        .map_err(|e| Error::refused(format!("reference manifest: {e}")))?;
    let settings_match = match product {
        Product::Reference(recipe) => {
            parsed.version == recipe.version && parsed.exposes == [recipe.name]
        }
        Product::Agit => parsed.exposes == ["agit"] && parsed.harness.is_none(),
    };
    let controls = product.controls();
    let mut agit_origin = None;
    if !settings_match
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
        let cohort_matches = match product {
            Product::Reference(recipe) => {
                receipt.source_cohort == super::witness(recipe, super::artifact(recipe, target)?)
            }
            Product::Agit => {
                crate::artifact::bottle::bottle_row("agit", target)?;
                let origin: agit::BuildOrigin =
                    aterm_json::from_value(receipt.source_cohort.clone())
                        .map_err(|e| Error::refused(format!("agit receipt source record: {e}")))?;
                origin.check(&parsed.version)?;
                // Reject unrecognized fields and mixtures of fork revisions across platform rows.
                let canonical = aterm_json::to_value(&origin)
                    .map_err(|e| Error::refused(format!("agit receipt source record: {e}")))?;
                receipt.source_cohort == canonical
                    && agit_origin.get_or_insert_with(|| origin.clone()) == &origin
            }
        };
        if receipt.schema != 1
            || receipt.evidence != "operator-native-runtime-observation"
            || receipt.status != "native-pass"
            || receipt.native_host.as_deref() != Some(artifact.target.as_str())
            || receipt.native_translation.as_deref() != Some("native")
            || receipt.control_count != receipt.controls.len()
            || receipt.control_count != controls.len()
            || !controls.iter().all(|name| {
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
            || !cohort_matches
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
    if !requires_native_receipts(bytes)? {
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
        // A merged agit pack uploads the newly packed asset and carries the previously
        // qualified local archives. Freeze every row before the ready manifest goes out.
        let archive = directory.join(&artifact.asset);
        if parsed.program == "agit" && !frozen.contains(&archive) {
            copy_snapshot(
                &original_manifest.with_file_name(&artifact.asset),
                &archive,
                aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND,
            )?;
            let before_manifest = frozen
                .iter()
                .position(|p| p == prepared_manifest)
                .ok_or_else(|| Error::refused("agit snapshot lost its ready manifest"))?;
            frozen.insert(before_manifest, archive);
        }
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
#[path = "qualification_tests.rs"]
mod tests;
