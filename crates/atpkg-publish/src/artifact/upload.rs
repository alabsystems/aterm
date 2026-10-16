// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A pack's upload to the staging owner's `atpkg-<program>-<build>` release:
//!
//! * every file is under the release host's 2 GiB per-asset ceiling, and the asset still
//!   hashes to the signed row (a rebuild racing between sign and upload would ship bytes
//!   every client refuses);
//! * a pack is immutable once public: when the public owner already serves the tag, the
//!   upload is a no-op if its manifest is byte-identical and a refusal otherwise (the
//!   manifest carries the tarball's sha256, so equal manifests are equal packs);
//! * the staging release is created as a prerelease, or its assets replaced
//!   (`Replace::Clobber`) to converge a partial publish.

use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::{HostError, Replace};

/// Before a `--upload` pack builds anything, check the staging owner's token file is there:
/// uploads write as that token, never gh's own login, so without it the pack would build
/// and sign only to be refused at the first write.
///
/// # Errors
///
/// A refusal naming the token file.
pub fn token_preflight(ctx: &Ctx) -> Result<()> {
    if std::fs::metadata(&ctx.tokens.staging).is_ok_and(|m| m.len() > 0) {
        return Ok(());
    }
    Err(Error::refused(format!(
        "--upload needs the staging owner's token at {} (every pack is uploaded to {} first, \
         and gh's own login never writes) — nothing built",
        ctx.shown(&ctx.tokens.staging),
        ctx.accounts.staging
    )))
}

/// The release tag of one program build.
#[must_use]
pub fn tag(program: &str, build: u64) -> String {
    format!("atpkg-{program}-{build}")
}

/// Refuse a file over the release host's per-asset ceiling
/// (`aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND`, which every downloader applies), and
/// an asset whose bytes no longer hash to the signed row's `sha256`.
///
/// # Errors
///
/// The refusal, before any release is touched.
pub fn preflight(asset: &Path, signed_sha256: &str, others: &[PathBuf]) -> Result<()> {
    for f in std::iter::once(asset).chain(others.iter().map(PathBuf::as_path)) {
        let len = std::fs::metadata(f)
            .map_err(|e| Error::io(format!("stat {}", f.display()), &e))?
            .len();
        if len > aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND {
            return Err(Error::refused(format!(
                "{} is {len} bytes, over the release host's 2 GiB per-asset limit",
                f.display()
            )));
        }
    }
    let got = atpkg::sha256_file(asset)
        .map_err(|e| Error::io(format!("hash {}", asset.display()), &e))?;
    if got != signed_sha256 {
        return Err(Error::refused(format!(
            "{} hashes to {got}, not the {signed_sha256} its manifest signed — refusing to \
             ship bytes that do not match the signed manifest",
            asset.display()
        )));
    }
    Ok(())
}

/// What an upload did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uploaded {
    /// The staging release was created, or its assets converged.
    Staged,
    /// The public owner already serves this tag with this very manifest: nothing to do.
    AlreadyPublic,
}

/// Upload `files` (the asset, the manifest `manifest` and its signature) to the staging
/// owner's release of `repo`'s `tag`.
///
/// # Errors
///
/// A refusal when the public owner serves the tag with a different manifest (or one that
/// cannot be fetched), or the host refuses; a could-not-run when an owner cannot be
/// reached.
pub fn publish(
    ctx: &Ctx,
    repo: &str,
    tag: &str,
    manifest: &Path,
    files: &[PathBuf],
    notes: &str,
) -> Result<Uploaded> {
    let staging = ctx.staging_slug(repo);
    let public = ctx.public_slug(repo);
    if public != staging {
        match ctx.hosts.channel.release(&public, tag) {
            Ok(_) => {
                let name = manifest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let tmp = aterm_tempfile::Builder::new()
                    .prefix("atpkg-publish-public-")
                    .tempdir()
                    .map_err(|e| Error::io("create a scratch dir", &e))?;
                ctx.hosts
                    .channel
                    .download(&public, tag, &[name.as_str()], tmp.path())
                    .map_err(|e| {
                        Error::refused(format!(
                            "{tag} is published on {public} but its manifest could not be \
                             fetched ({e}) — refusing to touch {staging}"
                        ))
                    })?;
                let theirs = std::fs::read(tmp.path().join(&name))
                    .map_err(|e| Error::io(format!("read the public {name}"), &e))?;
                let ours = std::fs::read(manifest)
                    .map_err(|e| Error::io(format!("read {}", manifest.display()), &e))?;
                if theirs == ours {
                    return Ok(Uploaded::AlreadyPublic);
                }
                return Err(Error::refused(format!(
                    "{tag} is PUBLISHED on {public} with a DIFFERENT manifest: a public pack \
                     is immutable. Bump the build number instead of re-publishing under this \
                     one"
                )));
            }
            Err(HostError::NotFound) => {}
            Err(e) => {
                return Err(Error::cannot_run(format!(
                    "could not ask {public} whether {tag} is public ({e}) — nothing was \
                     uploaded"
                )));
            }
        }
    }
    let host = &ctx.hosts.staging;
    let title = format!("atpkg {tag}");
    let converge = || {
        host.upload(&staging, tag, files, Replace::Clobber)
            .map_err(|e| Error::refused(format!("upload to {staging} {tag} failed: {e}")))
    };
    match host.release(&staging, tag) {
        Ok(_) => converge()?,
        Err(HostError::NotFound) => match host.create(&staging, tag, &title, notes, files) {
            Ok(()) => {}
            // Another run of this program created it meanwhile: converge onto it.
            Err(HostError::Refused(_)) => converge()?,
            Err(e) => {
                return Err(Error::refused(format!(
                    "creating {tag} on {staging} failed: {e}"
                )));
            }
        },
        Err(e) => {
            return Err(Error::cannot_run(format!(
                "could not read {staging} {tag}: {e}"
            )));
        }
    }
    Ok(Uploaded::Staged)
}
