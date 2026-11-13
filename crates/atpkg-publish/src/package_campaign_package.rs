// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native package qualification with create-only retention before host writes.

use std::io::{Read as _, Write as _};

use super::*;
use crate::artifact::package::{PackageOutcome, PackageRequest};
use crate::artifact::tools::Tools;
use crate::channel::{Log, Scratch};

const ARCHIVE_CAP: u64 = 4 * 1024 * 1024 * 1024;

impl Campaign {
    /// Qualify, sign, retain, stage and read back the exact package before
    /// recording staging. A prior receipt never substitutes for host readback.
    /// # Errors
    /// Native qualification/upload refusal, or different staging bytes.
    pub fn stage_package(
        &mut self,
        ctx: &Ctx,
        request: &PackageRequest,
        tools: &dyn Tools,
        inputs: &crate::project_toolchain::Inputs,
        signed_root: &dyn Fn(u64) -> std::result::Result<String, String>,
    ) -> Result<PackageOutcome> {
        let mut qualified = self.qualify_package(ctx, request, tools, inputs, signed_root)?;
        let (_, row, target) = package::package_row(&request.program)?;
        let pkg = package::read_package(&request.package, row.exposes[0], target)?;
        if Intent::from_package(ctx, &request.program, request.build, &pkg)? != self.inputs {
            return Err(Error::refused("the frozen package changed before staging"));
        }
        let files = [
            qualified.asset.clone(),
            qualified.manifest.clone(),
            qualified.signature.clone(),
        ];
        let expected = match self.entries.last().map(|entry| &entry.observation) {
            Some(Observation::Qualified { files }) => files.clone(),
            _ => {
                return Err(Error::refused(
                    "native qualification retained no package observation",
                ));
            }
        };
        if package_files(&qualified, &self.inputs.packed_tree)? != expected {
            return Err(Error::refused("qualified package changed before staging"));
        }
        crate::artifact::upload::preflight(&qualified.asset, &expected.archive, &files[1..])?;
        let tag = crate::artifact::upload::tag(&request.program, request.build);
        let uploaded = crate::artifact::upload::publish(
            ctx,
            &self.inputs.repository,
            &tag,
            &qualified.manifest,
            &files,
            &package::staging_notes(&request.program, &pkg, target),
        )?;
        let readback = Scratch::new("campaign-package-readback")?;
        let names = files
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| Error::refused("retained package file has no name"))
            })
            .collect::<Result<Vec<_>>>()?;
        if uploaded == crate::artifact::upload::Uploaded::AlreadyPublic {
            let owner = ctx.public_slug(&self.inputs.repository);
            for (name, file) in names.iter().zip(&files) {
                let (_, metadata) = crate::identity::open_regular_nonblock(file)
                    .map_err(|error| Error::io("bound public package readback", &error))?;
                match ctx.hosts.channel.fetch_public(
                    &owner,
                    &tag,
                    name,
                    &readback.path().join(name),
                    metadata.len(),
                ) {
                    crate::host::Probe::Present => {}
                    other => {
                        return Err(Error::cannot_run(format!(
                            "public package readback failed: {other:?}"
                        )));
                    }
                }
            }
        } else {
            crate::channel::download_retried(
                ctx.hosts.staging.as_ref(),
                &ctx.staging_slug(&self.inputs.repository),
                &tag,
                &names,
                readback.path(),
            )
            .map_err(|error| {
                Error::cannot_run(format!("staged package readback failed: {error}"))
            })?;
        }
        let downloaded = PackageOutcome {
            asset: readback.path().join(names[0]),
            manifest: readback.path().join(names[1]),
            signature: readback.path().join(names[2]),
            ..qualified.clone()
        };
        if package_files(&downloaded, &self.inputs.packed_tree)? != expected {
            return Err(Error::refused(
                "host package bytes differ from the retained qualification",
            ));
        }
        self.check()?;
        self.record(Observation::Staged { files: expected })?;
        qualified.uploaded = uploaded == crate::artifact::upload::Uploaded::Staged;
        Ok(qualified)
    }

    /// Publish a frozen package and its index as one resumable native operation.
    /// Index retries use the previously retained candidate, never a newly signed
    /// replacement. Installation and running-process adoption are separate.
    /// # Errors
    /// Every native package, staging, index or durable receipt failure.
    pub fn publish_package(
        &mut self,
        ctx: &Ctx,
        request: &PackageRequest,
        tools: &dyn Tools,
        inputs: &crate::project_toolchain::Inputs,
        signed_root: &dyn Fn(u64) -> std::result::Result<String, String>,
        log: &mut Log,
    ) -> Result<crate::channel::index::Outcome> {
        if self.retained_index(ctx)?.is_some() {
            self.qualify_package(ctx, request, tools, inputs, signed_root)?;
            return self.resume_index(ctx, log);
        }
        self.stage_package(ctx, request, tools, inputs, signed_root)?;
        let mut delta = crate::api::Delta::default();
        delta
            .pins
            .insert(self.inputs.program.clone(), self.inputs.build);
        let index_request = crate::api::IndexRequest {
            baseline: crate::api::Baseline::ChannelHead,
            delta,
            out: self.root.join("index-working"),
            upload: true,
            pack_dir: None,
        };
        let outcome = crate::channel::index::publish_with_observer(
            ctx,
            &index_request,
            log,
            &mut self.retention(ctx),
        )?;
        let retained = self
            .retained_index(ctx)?
            .ok_or_else(|| Error::refused("publication retained no signed candidate"))?;
        self.record(Observation::IndexPublished {
            build: outcome.published.index_build,
            quad: crate::channel::index::quad_digests(retained.candidate())?,
        })?;
        Ok(outcome)
    }

    /// Requalify the actual package with native source, compiler, tree, signing
    /// and client staging gates, retaining exact signed bytes before any upload.
    /// A retry producing different bytes refuses instead of replacing them.
    /// # Errors
    /// A changed frozen package, failed qualification, or ambiguous retention.
    pub fn qualify_package(
        &mut self,
        ctx: &Ctx,
        request: &PackageRequest,
        tools: &dyn Tools,
        inputs: &crate::project_toolchain::Inputs,
        signed_root: &dyn Fn(u64) -> std::result::Result<String, String>,
    ) -> Result<PackageOutcome> {
        self.check()?;
        if request.program != self.inputs.program
            || request.build != self.inputs.build
            || request.key.is_some()
            || request.toolchain.is_some()
        {
            return Err(Error::refused(
                "package qualification differs from the frozen campaign",
            ));
        }
        let (definition, row, target) = package::package_row(&request.program)?;
        let pkg = package::read_package(&request.package, row.exposes[0], target)?;
        if Intent::from_package(ctx, &request.program, request.build, &pkg)? != self.inputs {
            return Err(Error::refused(
                "the frozen package or publication destinations changed",
            ));
        }
        let scratch = Scratch::new("campaign-package-qualification")?;
        let mut native = request.clone();
        native.upload = false;
        native.out = scratch.path().join("out");
        let admitted =
            package::pack_package_with_signed_root(ctx, &native, tools, inputs, signed_root)?;
        if Intent::from_package(ctx, &request.program, request.build, &pkg)? != self.inputs {
            return Err(Error::refused(
                "the frozen package changed during qualification",
            ));
        }
        let root = self.root.join("qualified-package");
        super::index_snapshot::directory(&root)?;
        let manifest = retain(&root, &admitted.manifest, crate::identity::MAX_META_BYTES)?;
        let signature = retain(&root, &admitted.signature, atpkg::net::SIG_CAP)?;
        let asset = retain(&root, &admitted.asset, ARCHIVE_CAP)?;
        // Qualification's temporary files are not custody. Verify the exact
        // retained signature, archive and extracted tree again before recording.
        let signer = crate::artifact::pack::Signer::load(ctx)?;
        let read = |path: &Path| {
            crate::identity::read_regular_bytes(path, crate::identity::MAX_META_BYTES)
                .map_err(|error| Error::io("read retained signed package", &error))
        };
        let signed = signer
            .authority()
            .verify_pkg(&read(&manifest)?, &read(&signature)?)?;
        if signed.version != self.inputs.version
            || signed
                .artifacts
                .iter()
                .any(|artifact| artifact.tree_root != self.inputs.packed_tree)
        {
            return Err(Error::refused(
                "the retained signed package differs from the frozen version or tree",
            ));
        }
        package::check_pack(definition, &row, request.build, &signed, &|name| {
            if asset.file_name().and_then(|file| file.to_str()) != Some(name) {
                return Err(Error::refused("the retained package names another archive"));
            }
            Ok(asset.clone())
        })?;
        self.check()?;
        self.record(Observation::Qualified {
            files: PackageFiles {
                manifest: digest(&manifest, crate::identity::MAX_META_BYTES)?,
                signature: digest(&signature, atpkg::net::SIG_CAP)?,
                archive: digest(&asset, ARCHIVE_CAP)?,
                tree_root: self.inputs.packed_tree.clone(),
            },
        })?;
        Ok(PackageOutcome {
            manifest,
            signature,
            asset,
            ..admitted
        })
    }
}

fn package_files(package: &PackageOutcome, tree_root: &str) -> Result<PackageFiles> {
    Ok(PackageFiles {
        manifest: digest(&package.manifest, crate::identity::MAX_META_BYTES)?,
        signature: digest(&package.signature, atpkg::net::SIG_CAP)?,
        archive: digest(&package.asset, ARCHIVE_CAP)?,
        tree_root: tree_root.into(),
    })
}

fn retain(root: &Path, source: &Path, cap: u64) -> Result<PathBuf> {
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::refused("qualified package file has no native name"))?;
    let expected = digest(source, cap)?;
    private_file::write_private_new(root, name, &[], |destination| {
        let (source, metadata) = crate::identity::open_regular_nonblock(source)?;
        if metadata.len() > cap {
            return Err(std::io::Error::other(
                "qualified package exceeds retention bound",
            ));
        }
        let mut destination = destination;
        let count = std::io::copy(&mut source.take(cap + 1), &mut destination)?;
        if count != metadata.len() || count > cap {
            return Err(std::io::Error::other(
                "qualified package changed while retaining",
            ));
        }
        destination.flush()
    })
    .map_err(|error| Error::io("retain qualified package", &error))?;
    let path = root.join(name);
    let (file, _) = private_file::open_private(root, name)
        .map_err(|error| Error::refused(format!("inspect retained package: {error}")))?;
    if digest(&path, cap)? != expected {
        return Err(Error::refused(
            "retained package bytes differ from fresh native qualification",
        ));
    }
    aterm_durable::Flush::Durable
        .file(&file)
        .and_then(|()| aterm_durable::Flush::Durable.dir(root))
        .map_err(|error| Error::io("finish retained qualified package", &error))?;
    Ok(path)
}

fn digest(path: &Path, cap: u64) -> Result<String> {
    let (mut file, metadata) = crate::identity::open_regular_nonblock(path)
        .map_err(|error| Error::io("open qualified package file", &error))?;
    if metadata.len() > cap {
        return Err(Error::refused("qualified package exceeds retention bound"));
    }
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut count = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| Error::io("hash qualified package", &error))?;
        if read == 0 {
            break;
        }
        count += read as u64;
        if count > cap {
            return Err(Error::refused(
                "qualified package grew beyond retention bound",
            ));
        }
        hash.update(&buffer[..read]);
    }
    if count != metadata.len() {
        return Err(Error::refused("qualified package changed while hashing"));
    }
    Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
}
