// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit unsigned staging of retained GNU bottles. This path has no signer or indexer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::api::Packer;
use crate::artifact::bottle;
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::programs::Shape;
use crate::quad;
use crate::triple::{Os, Triple};

use super::LaneKind;
use super::rows::{self, Fetched};
use super::state::{LaneState, Lock};

/// Parse repeated explicit package inputs. This action never acquires a build or publish lane.
/// # Errors
/// A missing input, unsupported flag or empty package list.
pub fn parse(args: &[String]) -> Result<Vec<PathBuf>> {
    let mut packages = Vec::new();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        if flag != "--reuse-package" {
            return Err(Error::usage(
                "lane stage linux takes --reuse-package PKG (repeatable)",
            ));
        }
        let path = args
            .next()
            .filter(|path| !path.is_empty() && !path.starts_with('-'))
            .ok_or_else(|| Error::usage("--reuse-package needs a package manifest"))?;
        packages.push(PathBuf::from(path));
    }
    if packages.is_empty() {
        return Err(Error::usage("lane stage linux needs --reuse-package PKG"));
    }
    Ok(packages)
}

/// Verify the fresh public package base, admit retained archives, and stage exact unsigned
/// extensions for the normal Linux lane. Even a rostered caller never signs or uploads here.
/// # Errors
/// Busy state, an unverified/stale/downgraded head or package, mismatched selected pin,
/// conflicting staged row, or any archive/native qualification/publication failure.
pub fn stage(ctx: &Ctx, packages: &[PathBuf], packer: &dyn Packer) -> Result<Vec<PathBuf>> {
    if packages.is_empty() {
        return Err(Error::usage("no retained packages were supplied"));
    }
    let state = LaneState::of(LaneKind::Linux, &ctx.state_dir);
    state.ensure()?;
    let _guard = match state.try_lock()? {
        Lock::Held(guard) => guard,
        Lock::Busy => return Err(Error::refused("another Linux lane owns its staging state")),
    };
    let scratch = crate::channel::Scratch::new("reuse-linux-packages")?;
    let (number, head) = quad::fetch_head(ctx, &ctx.index_slug(), &scratch.path().join("head"))?;
    state.check_index(number)?;
    let stage_root = state.root.join(format!("staged-index-{number}"));
    let mut outputs = BTreeSet::new();
    for (input_number, input) in packages.iter().enumerate() {
        let package = bottle::reuse::read_package(input)?;
        let definition = crate::programs::get(&package.program)
            .ok_or_else(|| Error::refused("retained package is not a declared program"))?;
        let Shape::Bottle(bottle) = definition.shape else {
            return Err(Error::refused(
                "only declared GNU bottle packages can be reused",
            ));
        };
        let index_row =
            head.index.programs.get(&package.program).ok_or_else(|| {
                Error::refused("the fresh index does not name the retained program")
            })?;
        let targets: BTreeSet<_> = package
            .artifacts
            .iter()
            .filter_map(|row| Triple::parse(&row.target).ok())
            .filter(|target| target.os() == Os::Linux)
            .collect();
        if targets.is_empty() || targets.iter().any(|target| !bottle.linux.contains(target)) {
            return Err(Error::refused(
                "retained package has no declared GNU bottle target",
            ));
        }
        for target in &targets {
            let channel = head
                .index
                .channel_for(atpkg::config::CHANNEL, target.as_str())
                .ok_or_else(|| Error::refused("the fresh index serves no selected GNU channel"))?;
            if channel.pin.get(&package.program) != Some(&package.build_number) {
                return Err(Error::refused(
                    "retained package does not match the fresh GNU target pin",
                ));
            }
        }
        let work = scratch.path().join(input_number.to_string());
        let public = match rows::fetch_published(
            &ctx.hosts.channel,
            &ctx.public_slug(&index_row.repo),
            &package.program,
            package.build_number,
            &work.join("published"),
            &head,
        ) {
            Fetched::Present { path, .. } => path,
            Fetched::Absent(why) | Fetched::Unreachable(why) | Fetched::Invalid(why) => {
                return Err(Error::refused(format!(
                    "no verified published reuse base: {why}"
                )));
            }
        };
        let published = read(&public)?;
        let staged =
            super::linux::staged_manifest(&stage_root, &package.program, package.build_number);
        let mut base = match std::fs::symlink_metadata(&staged) {
            Ok(_) => {
                let bytes = read(&staged)?;
                if !bytes.starts_with(&published) {
                    return Err(Error::refused(
                        "the existing stage is not an extension of the fresh public package",
                    ));
                }
                bytes
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => published,
            Err(error) => return Err(Error::io("inspect existing package stage", &error)),
        };
        for target in targets {
            let base_path = work.join(format!("base-{target}.toml"));
            std::fs::write(&base_path, &base)
                .map_err(|e| Error::io("write reuse merge base", &e))?;
            // Every import still reaches the same archive/native qualification route. An
            // existing target is never overwritten, even when another invocation staged it.
            let result = packer.reuse_bottle(
                ctx,
                &bottle::reuse::Request {
                    manifest: input.clone(),
                    triple: target,
                    merge_from: base_path,
                    out: work.join(target.as_str()),
                },
            )?;
            if result.program != package.program
                || result.build != package.build_number
                || result.version != package.version
                || result.triple != target
                || result.signature.is_some()
                || result.uploaded
            {
                return Err(Error::refused(
                    "retained package preparation changed its selected identity",
                ));
            }
            let merged = read(&result.manifest)?;
            if !merged.starts_with(&base) {
                return Err(Error::refused(
                    "retained preparation did not preserve its exact merge base",
                ));
            }
            let parsed = rows::local(&result.manifest)?;
            let row = parsed
                .artifact_for(target.as_str())
                .ok_or_else(|| Error::refused("retained preparation omitted its selected row"))?;
            let directory = staged
                .parent()
                .ok_or_else(|| Error::refused("no package staging directory"))?;
            aterm_types::fs_restricted::ensure_private_dir(directory)
                .map_err(|e| Error::io("create private package stage", &e))?;
            let destination = directory.join(&row.asset);
            match std::fs::symlink_metadata(&destination) {
                Ok(meta) if meta.is_file() && meta.len() == row.size => {
                    if atpkg::tree::sha256_file(&destination)
                        .map_err(|e| Error::io("read existing retained archive", &e))?
                        != row.sha256
                    {
                        return Err(Error::refused(
                            "different bytes already occupy the retained asset's staging path",
                        ));
                    }
                }
                Ok(_) => {
                    return Err(Error::refused(
                        "an unsafe or different entry occupies the retained asset's staging path",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    crate::identity::copy_regular_file(&result.asset, &destination)
                        .map_err(|e| Error::io("copy admitted retained asset", &e))?;
                }
                Err(error) => {
                    return Err(Error::io("inspect retained archive destination", &error));
                }
            }
            if atpkg::tree::sha256_file(&destination)
                .map_err(|e| Error::io("recheck staged archive", &e))?
                != row.sha256
            {
                return Err(Error::refused(
                    "the retained archive changed before staging",
                ));
            }
            if merged != base || !staged.exists() {
                aterm_uds::atomic_file::write(&staged, &merged, 0o600)
                    .map_err(|e| Error::io("publish unsigned retained package stage", &e))?;
                match std::fs::remove_file(staged.with_extension("toml.sig")) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(Error::io("remove superseded stage signature", &error));
                    }
                }
            }
            base = merged;
            outputs.insert(staged.clone());
        }
    }
    Ok(outputs.into_iter().collect())
}

fn read(path: &Path) -> Result<Vec<u8>> {
    crate::identity::read_regular_bytes(path, crate::identity::MAX_META_BYTES)
        .map_err(|error| Error::io(format!("read {}", path.display()), &error))
}
