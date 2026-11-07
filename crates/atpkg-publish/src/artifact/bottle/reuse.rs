// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reuse an admitted GNU source bottle's exact archive, without rebuilding or repacking it.

use std::path::PathBuf;

use super::BottleOutcome;
use crate::artifact::pkgdoc::{self, BottleRow, Header, Kind};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::programs::{BottleOrigin, Shape};
use crate::triple::{Os, Triple};

/// One retained artifact and the verified published manifest it extends.
#[derive(Clone, Debug)]
pub struct Request {
    /// The retained package manifest; its archive must be beside it.
    pub manifest: PathBuf,
    /// The declared GNU target to reuse.
    pub triple: Triple,
    /// The verified package bytes, or their previously admitted unsigned extension.
    pub merge_from: PathBuf,
    /// Private output directory for the copied archive and unsigned merged manifest.
    pub out: PathBuf,
}

/// Admit the archive and repeat the existing native GNU qualification on its exact bytes.
/// No compiler, packer, signer or host write runs.
/// # Errors
/// Unsupported shape, incompatible metadata, unsafe/missing archive or any admission refusal.
pub fn prepare(_ctx: &Ctx, request: &Request) -> Result<BottleOutcome> {
    prepare_with(request, super::upstream::qualify_staged)
}

pub(super) fn prepare_with(
    request: &Request,
    qualify: impl FnOnce(&BottleOutcome, &std::path::Path, &atpkg::manifest::Artifact) -> Result<()>,
) -> Result<BottleOutcome> {
    let package = read_package(&request.manifest)?;
    let definition = crate::programs::get(&package.program)
        .ok_or_else(|| Error::refused("retained package is not a declared program"))?;
    let Shape::Bottle(bottle) = definition.shape else {
        return Err(Error::refused("retained package is not an upstream bottle"));
    };
    if request.triple.os() != Os::Linux
        || !bottle.linux.contains(&request.triple)
        || !matches!(bottle.origin, BottleOrigin::Formula)
    {
        return Err(Error::refused(
            "retained package has no GNU source-bottle qualification route",
        ));
    }
    let selected: Vec<_> = package
        .artifacts
        .iter()
        .filter(|row| row.target == request.triple.as_str())
        .collect();
    let [artifact] = selected.as_slice() else {
        return Err(Error::refused(
            "retained package must name the selected target exactly once",
        ));
    };
    // This is the bottle producer's shape, not an importer that silently drops vendor apply
    // instructions or compiler provenance when rendering the new row.
    if i64::from(package.schema) != pkgdoc::SCHEMA
        || !package.requires.is_empty()
        || !bottle_shape(artifact)
        || artifact.asset
            != pkgdoc::asset_name(&package.program, package.build_number, request.triple)
    {
        return Err(Error::refused(
            "retained row is not an unchanged GNU bottle artifact",
        ));
    }
    let header = Header {
        program: package.program.clone(),
        version: package.version.clone(),
        build: package.build_number,
        exposes: package.exposes.clone(),
        shim_env: package.shim_env.clone(),
    };
    let row = BottleRow {
        triple: request.triple,
        kind: Kind::ElfBinary,
        asset: artifact.asset.clone(),
        sha256: artifact.sha256.clone(),
        tree_root: artifact.tree_root.clone(),
        size: artifact.size,
        disk_installed: artifact.cost.disk_installed,
    };
    // Reuse the producer's required digest/tree-root/size/header checks even when this
    // exact row is already staged and therefore needs no second append.
    pkgdoc::fresh_bottle(&header, &row)?;
    let base =
        crate::identity::read_regular_bytes(&request.merge_from, crate::identity::MAX_META_BYTES)
            .map_err(|e| Error::io("read verified reuse base", &e))?;
    let parsed_base: atpkg::PkgManifest = aterm_toml::from_str(
        std::str::from_utf8(&base).map_err(|_| Error::refused("reuse base is not UTF-8"))?,
    )
    .map_err(|e| Error::refused(format!("reuse base is not a package: {e}")))?;
    let already = parsed_base
        .artifacts
        .iter()
        .filter(|a| a.target == request.triple.as_str())
        .count();
    let merged = if already != 0 {
        if already != 1 {
            return Err(Error::refused("reuse base repeats the selected target"));
        }
        if !bottle_shape(
            parsed_base
                .artifact_for(request.triple.as_str())
                .expect("counted above"),
        ) {
            return Err(Error::refused(
                "the existing target has different apply semantics",
            ));
        }
        pkgdoc::check_parsed_bottle(&parsed_base, &header, &row)?;
        base
    } else {
        pkgdoc::merge_bottle(base, &header, &row)?.bytes().to_vec()
    };
    let source = request
        .manifest
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(&row.asset);
    let (file, metadata) = aterm_uds::regular_file::open(&source)
        .map_err(|e| Error::io("open retained archive", &e))?;
    admit_input(&metadata)?;
    if metadata.len() != row.size || row.size > aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND {
        return Err(Error::refused(
            "retained archive size differs from its bounded manifest row",
        ));
    }
    std::fs::create_dir_all(&request.out)
        .map_err(|e| Error::io("create retained package output", &e))?;
    let asset = request.out.join(&row.asset);
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut writer = options
        .open(&asset)
        .map_err(|e| Error::io("create private retained archive snapshot", &e))?;
    let copied = std::io::copy(&mut file.take(row.size.saturating_add(1)), &mut writer)
        .map_err(|e| Error::io("snapshot retained archive", &e))?;
    drop(writer);
    if copied != row.size {
        return Err(Error::refused(
            "retained archive changed size while being copied",
        ));
    }
    let manifest = request
        .out
        .join(pkgdoc::manifest_name(&header.program, header.build));
    std::fs::write(&manifest, &merged)
        .map_err(|e| Error::io("write unsigned reuse manifest", &e))?;
    let outcome = BottleOutcome {
        program: header.program,
        build: header.build,
        version: header.version,
        triple: request.triple,
        asset,
        manifest,
        signature: None,
        uploaded: false,
    };
    let stage = crate::channel::Scratch::new("retained-bottle-admission")?;
    let tree = stage.path().join("stage with spaces");
    atpkg::verify_for_publication(
        artifact,
        &outcome.asset,
        &tree,
        &atpkg::install::StageHooks::NONE,
    )
    .map_err(|e| Error::refused(format!("retained GNU artifact admission: {e}")))?;
    qualify(&outcome, &tree, artifact)?;
    Ok(outcome)
}

fn bottle_shape(artifact: &atpkg::manifest::Artifact) -> bool {
    artifact.kind == "elf-binary"
        && artifact.protocol == "github-release"
        && artifact.sha512.is_empty()
        && artifact.url.is_empty()
        && artifact.payload.is_empty()
        && artifact.entry.is_empty()
        && artifact.strip_components == 0
        && artifact.links.is_empty()
        && artifact.vendor.is_empty()
        && artifact.provenance.is_none()
        && artifact.reloc == "self-contained"
        && artifact.cost.download_bytes == artifact.size
        && artifact.cost.build_seconds == 0
}

fn admit_input(metadata: &std::fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != aterm_uds::peer::our_uid() || metadata.mode() & 0o6022 != 0 {
            return Err(Error::refused(
                "retained package input must be owned and not writable by others",
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

pub(crate) fn read_package(path: &std::path::Path) -> Result<atpkg::PkgManifest> {
    use std::io::Read as _;
    let (file, metadata) = aterm_uds::regular_file::open(path)
        .map_err(|e| Error::io("open retained package manifest", &e))?;
    admit_input(&metadata)?;
    let limit = crate::identity::MAX_META_BYTES;
    if metadata.len() > limit {
        return Err(Error::refused("retained package manifest is too large"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io("read retained package manifest", &e))?;
    if bytes.len() as u64 > limit {
        return Err(Error::refused(
            "retained package manifest grew beyond its byte bound",
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::refused("retained package manifest is not UTF-8"))?;
    aterm_toml::from_str(text)
        .map_err(|e| Error::refused(format!("retained package manifest is malformed: {e}")))
}
