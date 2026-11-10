// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Uploading signed `atpkg-<program>-<build>` release assets to the staging and public hosts.
use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::rest::STAGED;
use crate::host::{HostError, HostRole, Release, ReleaseHost, Replace, Slug, gh_serves};

#[path = "upload_version.rs"]
mod version;
pub use version::VERSION_RESERVATION;

/// Check that the staging owner can be written before building: its token file exists and is
/// non-empty where the staging host is `RestHost` (`host::gh_serves`), and otherwise the staging
/// host's own pre-claim check (`write_preflight`, finding F2) of the staging index repository.
/// A pack verb asks [`program_preflight`] instead, of the repository its upload goes to.
/// # Errors
/// Returns a refusal naming the missing or unusable token file, or the staging writer the host
/// refused.
pub fn token_preflight(ctx: &Ctx) -> Result<()> {
    token_preflight_for(ctx, atpkg::manifest::INDEX_REPO)
}

/// [`token_preflight`] of the staging repository `repo`, the one `publish` writes a pack of
/// that program to: under agh both push and a release-tier grant are per repository, so the
/// pre-claim check asks it of the repository the upload goes to.
/// # Errors
/// Returns a refusal naming the missing or unusable token file, or the staging writer the host
/// refused on `repo`.
pub fn token_preflight_for(ctx: &Ctx, repo: &str) -> Result<()> {
    if !gh_serves(ctx, HostRole::Staging) {
        let slug = ctx.staging_slug(repo);
        return ctx.hosts.staging.write_preflight(&slug).map_err(|e| {
            Error::refused(format!(
                "--upload needs the staging owner's writer on {slug}: {e} — nothing built"
            ))
        });
    }
    match ctx.usable_token(&ctx.tokens.staging) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::refused(format!(
            "--upload needs the staging owner's token at {} (every pack is uploaded to {} first, \
             and a write never goes anonymously) — nothing built",
            ctx.shown(&ctx.tokens.staging),
            ctx.accounts.staging
        ))),
        Err(why) => Err(Error::refused(format!(
            "--upload needs the staging owner's token at {}: {why} — nothing built",
            ctx.shown(&ctx.tokens.staging),
        ))),
    }
}

/// [`token_preflight_for`] of the staging repository `program`'s packs are uploaded to
/// (`programs::get(program).repo`): what `pack`, `pack-bundle`, `pack-bottle` and
/// `pack-package --upload` check before anything is built. A program the channel does not pin
/// asks nothing here, so the verb's own usage error names it.
/// # Errors
/// As [`token_preflight_for`].
pub fn program_preflight(ctx: &Ctx, program: &str) -> Result<()> {
    match crate::programs::get(program) {
        Some(def) => token_preflight_for(ctx, def.repo),
        None => Ok(()),
    }
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
        let len = std::fs::symlink_metadata(f)
            .map_err(|e| Error::io(format!("stat {}", f.display()), &e))?
            .len();
        if len > aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND {
            return Err(Error::refused(format!(
                "{} is {len} bytes, over the release host's 2 GiB per-asset limit",
                f.display()
            )));
        }
    }
    if let Some((program, build)) = managed_pack(tag) {
        if owner == "public" {
            return Err(Error::refused(
                "raw public upload cannot replace a managed pack; use `atpkg-publish mirror <quad-dir>` or `atpkg-publish index --upload` for an immutable pack. For a signed Linux target extension, resume the installed Linux lane on its rostered machine: `atpkg-publish lane run linux --sha256 <installed-snapshot-sha256>`; it verifies the signed byte-prefix extension before guarded owner uploads",
            ));
        }
        let expected = super::pkgdoc::manifest_name(program, build);
        let manifests: Vec<_> = files
            .iter()
            .filter(|file| {
                file.file_name().and_then(|name| name.to_str()) == Some(expected.as_str())
            })
            .collect();
        let [manifest] = manifests.as_slice() else {
            return Err(Error::refused(format!(
                "managed {tag} upload needs exactly one supplied {expected}"
            )));
        };
        version::check_managed_manifest(manifest, program, build)?;
        match publish(ctx, repo, tag, manifest, &files, "managed pack upload")? {
            Uploaded::Staged => println!("staged managed pack {slug} {tag}"),
            Uploaded::AlreadyPublic => println!("{tag} already has this immutable public manifest"),
        }
        return Ok(());
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

/// Pack tags use the program and canonical build number. The channel index has
/// its own immutable quad protocol and is not a program pack.
fn managed_pack(tag: &str) -> Option<(&str, u64)> {
    let (program, number) = tag.strip_prefix("atpkg-")?.rsplit_once('-')?;
    if program.is_empty() || program == "index" {
        return None;
    }
    let build = number.parse::<u64>().ok()?;
    (number == build.to_string()).then_some((program, build))
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
        let len = std::fs::symlink_metadata(f)
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

/// Upload `files` to the staging release of `repo`'s `tag` after checking public immutability,
/// never overwriting a staged manifest of another version (D51).
/// # Errors
/// Returns a refusal if the public release differs (or its manifest cannot be fetched), the
/// staged manifest records another version or none readable, or upload fails; cannot-run if
/// unreachable or the staged manifest cannot be downloaded.
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
                let theirs = version::read_manifest(&tmp.path().join(&name))?;
                let ours = version::read_manifest(manifest)?;
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
            // This identity cannot list the public owner's releases. Two repository 404s
            // establish only that neither this identity nor an anonymous reader sees it:
            // a missing repository and a private one hidden from both look the same.
            // Either permits this private dev staging (§11 D50), not a public publication;
            // any other answer keeps the could-not-run.
            Err(HostError::Unreachable(_))
                if matches!(ctx.hosts.channel.repo_seen(&public), Ok(false)) => {}
            Err(e) => {
                return Err(Error::cannot_run(format!(
                    "could not ask {public} whether {tag} is public ({e}) — nothing was \
                     uploaded"
                )));
            }
        }
    }
    publish_pack_to(
        ctx.hosts.staging.as_ref(),
        &staging,
        tag,
        manifest,
        files,
        notes,
    )?;
    Ok(Uploaded::Staged)
}

/// Apply cross-host exact-version admission for one pack owner. The caller retains
/// its owner policy: generic staging checks public immutability, while the Linux
/// lane first verifies its signed byte-prefix target extension through the roster.
pub(crate) fn publish_pack_to(
    host: &dyn ReleaseHost,
    owner: &Slug,
    tag: &str,
    manifest: &Path,
    files: &[PathBuf],
    notes: &str,
) -> Result<()> {
    let reserved = reserve_pack_version(host, owner, tag, manifest, files, notes)?;
    host.upload(owner, tag, reserved.files(), Replace::Clobber)
        .map_err(|error| Error::refused(format!("upload to {owner} {tag} failed: {error}")))?;
    if reserved.release.draft {
        host.publish_draft(owner, tag)
            .map_err(|error| Error::refused(format!("publish {owner} {tag}: {error}")))?;
    }
    Ok(())
}

/// The admitted manifest copy and its lifetime, with payloads before ready metadata.
pub(crate) struct ReservedPack {
    prepared: version::Prepared,
    pub(crate) release: Release,
}

impl ReservedPack {
    pub(crate) fn files(&self) -> &[PathBuf] {
        &self.prepared.files
    }
}

/// Reserve one exact version without choosing the owner's payload replacement policy.
/// The mirror keeps `Never`; the validated Linux extension keeps `Clobber`.
pub(crate) fn reserve_pack_version(
    host: &dyn ReleaseHost,
    owner: &Slug,
    tag: &str,
    manifest: &Path,
    files: &[PathBuf],
    notes: &str,
) -> Result<ReservedPack> {
    let prepared = version::Prepared::new(manifest, files)?;
    let title = if let Some((program, build)) = managed_pack(tag) {
        version::check_managed_manifest_bytes(
            &prepared.manifest,
            &prepared.manifest_bytes,
            program,
            build,
        )?;
        let product = atpkg::alab::agent(program).map_or(program, |agent| agent.product);
        format!("{product} {} (build {build})", prepared.version)
    } else {
        format!("atpkg {tag}")
    };
    let unread = |e: HostError| Error::cannot_run(format!("could not read {owner} {tag}: {e}"));
    let before = match host.release(owner, tag) {
        Ok(release) => release,
        Err(HostError::NotFound) => {
            // Only independent reservation metadata precedes the archives. A racing
            // create or a lost response is reconciled by reading the actual release.
            let made = host.create(
                owner,
                tag,
                &title,
                notes,
                std::slice::from_ref(&prepared.reservation),
            );
            // A refused create converges onto the release a racing publisher made, and onto
            // nothing else: with none there, the refusal was the host's own (finding F1), so
            // it stands, in the create's own words.
            match (host.release(owner, tag), made) {
                (Ok(release), _) => release,
                (Err(HostError::NotFound), Err(refused @ HostError::Refused(_))) => {
                    return Err(Error::refused(format!(
                        "creating {tag} on {owner} failed: {refused}"
                    )));
                }
                (Err(error), made) => {
                    return Err(Error::cannot_run(format!(
                        "could not reconcile creating {owner} {tag} ({error}; create: {made:?}) — no payload uploaded"
                    )));
                }
            }
        }
        Err(e) => return Err(unread(e)),
    };
    check_staged(host, owner, tag, &before, &prepared)?;
    prepared.acquire(host, owner, tag)?;
    let current = host.release(owner, tag).map_err(unread)?;
    check_staged(host, owner, tag, &current, &prepared)?;
    Ok(ReservedPack {
        prepared,
        release: current,
    })
}

/// Validate known reservation metadata against an already verified manifest version.
pub(crate) fn check_version_reservation(path: &Path, version: &str) -> Result<()> {
    version::check_reservation(path, version)
}

/// The `version` a pkg manifest's bytes record: a TOML document's top-level string `version`.
fn manifest_version(bytes: &[u8]) -> Option<String> {
    let doc: aterm_toml::Value = std::str::from_utf8(bytes).ok()?.parse().ok()?;
    doc.get("version")?.as_str().map(str::to_string)
}

/// Before an upload clobbers the staging release `release` of `tag`, every manifest it already
/// stages under `manifest`'s name must be `manifest`'s bytes or record its `version` (a
/// rebuild, or a manifest merged with a second target): one build number names one version
/// (D51). A replace the REST host cut short between deleting the old manifest and renaming its
/// replacement leaves the replacement under that name plus [`STAGED`] alone, and that copy is
/// then what the release stages, so it is read too; so is a temporary asset agh's clobber left
/// (`{name}.agh-{8 hex}`, `agh::github::releases::is_clobber_temporary_of`). A release with
/// none of them holds nothing to keep.
/// # Errors
/// A refusal when a staged manifest records another version or none readable; a could-not-run
/// when one cannot be downloaded.
fn check_staged(
    host: &dyn ReleaseHost,
    staging: &Slug,
    tag: &str,
    release: &Release,
    prepared: &version::Prepared,
) -> Result<()> {
    let name = prepared
        .manifest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let present: Vec<String> = [name.clone(), format!("{name}{STAGED}")]
        .into_iter()
        .filter(|n| release.assets.contains(n))
        .chain(
            release
                .assets
                .iter()
                .filter(|asset| agh::github::releases::is_clobber_temporary_of(asset, &name))
                .cloned(),
        )
        .collect();
    if present.is_empty() {
        return Ok(());
    }
    let ours = &prepared.manifest_bytes;
    let version = &prepared.version;
    let tmp = aterm_tempfile::Builder::new()
        .prefix("atpkg-publish-staged-")
        .tempdir()
        .map_err(|e| Error::io("create a scratch dir", &e))?;
    for staged in &present {
        host.download(staging, tag, &[staged.as_str()], tmp.path())
            .map_err(|e| {
                Error::cannot_run(format!(
                    "could not read the {staged} {staging} {tag} already stages ({e}) — no \
                     payload was uploaded"
                ))
            })?;
        let theirs = version::read_manifest(&tmp.path().join(staged))?;
        if &theirs == ours {
            continue;
        }
        let at = match manifest_version(&theirs) {
            Some(v) if &v == version => continue,
            Some(v) => format!("at version `{v}`"),
            None => "recording no readable version".to_string(),
        };
        return Err(Error::refused(format!(
            "{tag} on {staging} already stages {staged} {at}, and this upload is version \
             `{version}` — one build number names one version, so the staged pack is not \
             overwritten (D51). If the staged one is wrong, delete that staging release by hand; \
             otherwise pack under another build number"
        )));
    }
    Ok(())
}
