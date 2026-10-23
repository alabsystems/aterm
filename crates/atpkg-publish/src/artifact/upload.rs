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
//!   (`Replace::Clobber`) to converge a partial publish;
//! * the `upload` verb ([`run`]) replaces assets of a release that already stands, on the
//!   staging or the public owner, as that owner's token: the Linux lane's signing ceremony
//!   re-uploads a manifest and its signature with it.

use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::{HostError, Replace};

/// Before a `--upload` pack builds anything, check the staging owner's token file is there:
/// uploads write as that token and never anonymously, so without it the pack would build
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
         and a write never goes anonymously) — nothing built",
        ctx.shown(&ctx.tokens.staging),
        ctx.accounts.staging
    )))
}

/// `upload <staging|public> <repo> <tag> <file>…`: replace (or add) each file's asset on the
/// release `tag` of that owner's `repo`, as that owner's token. The release must stand; this
/// creates none.
///
/// # Errors
///
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
