// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Uploading signed `atpkg-<program>-<build>` release assets to the staging and public hosts.
use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::{HostError, Replace};

/// Check that the staging owner's token file exists and is non-empty before building.
/// # Errors
/// Returns a refusal naming the missing token file.
pub fn token_preflight(ctx: &Ctx) -> Result<()> {
    if std::fs::metadata(&ctx.tokens.staging).is_ok_and(|m| m.len() > 0) {
        return Ok(());
    }
    Err(Error::refused(format!(
        "--upload needs the staging owner's token at {} (every pack is uploaded to {} first, \
         and a write never goes anonymously) — nothing built",
        ctx.shown(&ctx.tokens.staging),
        ctx.accounts.staging
    )))
}

/// Replace or add release assets on `<staging|public> <repo> <tag>`.
/// # Errors
/// A usage error for a missing operand or an unknown owner; a refusal for a file over the
/// per-asset ceiling or the host's refusal; a could-not-run when the owner cannot be reached.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let [owner, repo, tag, files @ ..] = args else {
        return Err(Error::usage(
            "upload needs <staging|public> <repo> <tag> <file>…",
        ));
    };
    if files.is_empty() {
        return Err(Error::usage("upload needs at least one file"));
    }
    let (host, slug) = match owner.as_str() {
        "staging" => (&ctx.hosts.staging, ctx.staging_slug(repo)),
        "public" => (&ctx.hosts.channel, ctx.public_slug(repo)),
        other => {
            return Err(Error::usage(format!(
                "upload: the owner is `staging` or `public`, not `{other}`"
            )));
        }
    };
    let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
    for f in &files {
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
    match host.upload(&slug, tag, &files, Replace::Clobber) {
        Ok(()) => {
            println!("uploaded {} asset(s) to {slug} {tag}", files.len());
            Ok(())
        }
        Err(HostError::Unreachable(why)) => {
            Err(Error::cannot_run(format!("upload to {slug} {tag}: {why}")))
        }
        Err(e) => Err(Error::refused(format!(
            "upload to {slug} {tag} failed: {e}"
        ))),
    }
}

/// Release tag for one program build (`atpkg-<program>-<build>`).
#[must_use]
pub fn tag(program: &str, build: u64) -> String {
    format!("atpkg-{program}-{build}")
}

/// Verify per-asset size bounds and that `asset` still hashes to `signed_sha256`.
/// # Errors
/// Returns a refusal if any file exceeds the size bound or `asset` mismatches `signed_sha256`.
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

/// Outcome of [`publish`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uploaded {
    /// The staging release was created, or its assets converged.
    Staged,
    /// The public owner already serves this tag with this very manifest: nothing to do.
    AlreadyPublic,
}

/// Upload `files` to the staging release of `repo`'s `tag` after checking public immutability.
/// # Errors
/// Returns a refusal if the public release differs (or its manifest cannot be fetched) or upload
/// fails, or cannot-run if unreachable.
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
