// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish mirror <quad-dir>`: make every pack a signed index pins public on the
//! channel owner. [`super::index`] runs it before it publishes; the verb is for a dry run
//! and a convergence by hand.
//!
//! The work list is [`crate::pins::union`] of the verified quad (`pin` and every overlay).
//! Per `(program, build)`:
//! 1. the public copy, read as a client reads it (download host, no credential): the
//!    manifest pair verified under the quad's roster
//!    ([`crate::quad::VerifiedQuad::verify_pkg`]), bound to this program and build, and
//!    every release asset it names probed; a complete public copy is skipped, with no
//!    staging read;
//! 2. else the staging copy, downloaded (retried) and checked ([`check_staged`]); a public
//!    manifest that exists must be byte-identical to staging's (a public pack is
//!    immutable);
//! 3. publish: create the release ([`public_notes`]: for an ALab package, the upstream base
//!    its staged archive records), or upload exactly what is missing (never replacing); a
//!    public release that serves no manifest (an orphaned draft) is refused;
//! 4. confirm every asset answers on the download host ([`super::probe_served`]).
//!
//! An answer the download host does not give ([`Probe::Unknown`]) is never read as
//! "absent": nothing is decided about that pack, and the run stops.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use atpkg::net::MANIFEST_CAP;

use super::{Log, download_retried, flag_value, host_error, probe_served};
use crate::artifact::{package, pkgdoc, upload};
use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::{HostError, Probe, Replace};
use crate::quad::{self, VerifiedQuad};

/// How the mirror runs.
#[derive(Clone, Debug)]
pub struct MirrorOptions {
    /// Verify and plan; write nothing to any host.
    pub dry_run: bool,
}

impl MirrorOptions {
    /// Publish what is missing.
    #[must_use]
    pub fn publish() -> Self {
        Self { dry_run: false }
    }

    /// Verify and plan only.
    #[must_use]
    pub fn dry_run() -> Self {
        Self { dry_run: true }
    }
}

/// What the mirror did, per release tag.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MirrorReport {
    /// Public and complete: nothing read from staging, nothing written.
    pub skipped: Vec<String>,
    /// Created on the public owner.
    pub created: Vec<String>,
    /// Completed on the public owner: the tag and what was uploaded.
    pub converged: Vec<(String, Vec<String>)>,
    /// A dry run's plan: the tag and what would be written.
    pub planned: Vec<(String, Vec<String>)>,
}

/// The release assets a verified manifest names: its `github-release` rows (an `https`
/// row's bytes live on the vendor's host and are never mirrored).
fn release_assets(pkg: &atpkg::PkgManifest) -> Vec<(String, String)> {
    pkg.artifacts
        .iter()
        .filter(|a| a.protocol.is_empty() || a.protocol == "github-release")
        .map(|a| (a.asset.clone(), a.sha256.clone()))
        .collect()
}

fn bind(pkg: &atpkg::PkgManifest, program: &str, build: u64, which: &str, tag: &str) -> Result<()> {
    if pkg.program == program && pkg.build_number == build {
        Ok(())
    } else {
        Err(Error::refused(format!(
            "{which} {tag} manifest binds program={} build={}, the index pins {program}/{build} \
             — refusing",
            pkg.program, pkg.build_number
        )))
    }
}

/// The staging release in `dir`, verified: the signed manifest pair under the client's
/// rule, the program/build bind, every release asset's sha256, and exactly the signed set.
/// Returns the expected file names, manifest pair first, and the verified manifest.
///
/// # Errors
///
/// A refusal naming what does not hold.
pub fn check_staged(
    quad: &VerifiedQuad,
    dir: &Path,
    staged: &[String],
    program: &str,
    build: u64,
) -> Result<(Vec<String>, atpkg::PkgManifest)> {
    let tag = upload::tag(program, build);
    let mf = pkgdoc::manifest_name(program, build);
    let sig = pkgdoc::sig_name(program, build);
    let (Ok(bytes), Ok(sig_bytes)) = (std::fs::read(dir.join(&mf)), std::fs::read(dir.join(&sig)))
    else {
        return Err(Error::refused(format!(
            "{tag} on staging lacks the signed pkg manifest pair ({mf}, {sig})"
        )));
    };
    let pkg = quad.verify_pkg(&bytes, &sig_bytes)?;
    bind(&pkg, program, build, "the staging", &tag)?;
    if pkg.artifacts.is_empty() {
        return Err(Error::refused(format!("{tag} manifest names no artifacts")));
    }
    let mut expected = vec![mf, sig];
    for (asset, want) in release_assets(&pkg) {
        let path = dir.join(&asset);
        if !staged.contains(&asset) || !path.is_file() {
            return Err(Error::refused(format!(
                "{tag} manifest names asset {asset} but the staging release lacks it"
            )));
        }
        let got = atpkg::sha256_file(&path).map_err(|e| Error::io(format!("hash {asset}"), &e))?;
        if got != want {
            return Err(Error::refused(format!(
                "{tag} asset {asset} hash mismatch vs the signed manifest"
            )));
        }
        if !expected.contains(&asset) {
            expected.push(asset);
        }
    }
    // By exact name: an asset name is never a pattern.
    if let Some(extra) = staged.iter().find(|f| !expected.contains(f)) {
        return Err(Error::refused(format!(
            "{tag} staging release carries unverifiable extra file {extra} — refusing to \
             republish it"
        )));
    }
    Ok((expected, pkg))
}

/// What every public pack release says about its authenticity.
pub const PUBLIC_NOTES: &str = "Signed atpkg package, published byte-identical from the \
     staging registry by atpkg-publish. Authenticity is the Ed25519 signature chain from the \
     paper master, through the machine roster published beside the index — the host is never \
     an authenticity input.";

/// The notes the public release of `program`'s verified staging pack `pkg` (its files in
/// `dir`) is created with: [`PUBLIC_NOTES`], then, for an ALab package whose record names
/// one, its upstream base (`Based on upstream openai/codex …`), read off the archive as a
/// client stages it ([`package::staged_upstream`]) and never off the staging release's
/// text, which nothing signs.
///
/// # Errors
///
/// A refusal when an ALab package's archive does not stage.
pub fn public_notes(program: &str, pkg: &atpkg::PkgManifest, dir: &Path) -> Result<String> {
    let upstream = package::staged_upstream(program, pkg, dir)?;
    Ok(match package::upstream_line(program, upstream.as_ref()) {
        Some(line) => format!("{PUBLIC_NOTES}\n\n{line}"),
        None => PUBLIC_NOTES.to_string(),
    })
}

/// Make every pack `quad` pins public on the channel owner.
///
/// # Errors
///
/// The first pack that is not, or cannot be made, public — with why. Nothing about a
/// pack is decided from a download host that did not answer.
pub fn mirror(
    ctx: &Ctx,
    quad: &VerifiedQuad,
    opts: &MirrorOptions,
    log: &mut Log,
) -> Result<MirrorReport> {
    let work = quad.dir.join(".mirror");
    let _ = std::fs::remove_dir_all(&work);
    let result = mirror_in(ctx, quad, opts, log, &work);
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn mirror_in(
    ctx: &Ctx,
    quad: &VerifiedQuad,
    opts: &MirrorOptions,
    log: &mut Log,
    work: &Path,
) -> Result<MirrorReport> {
    let mut work_list: BTreeSet<(String, u64)> = BTreeSet::new();
    let mut triples: BTreeSet<&str> = BTreeSet::new();
    for c in &quad.index.channels {
        work_list.extend(crate::pins::union(c));
        triples.extend(c.pin_by_target.keys().map(String::as_str));
    }
    if !triples.is_empty() {
        log.say(format!(
            "the index carries per-target pin overlays for: {} — their builds are in the work \
             list",
            triples.into_iter().collect::<Vec<_>>().join(" ")
        ));
    }
    let mut report = MirrorReport::default();
    for (program, build) in &work_list {
        let repo = quad
            .index
            .programs
            .get(program)
            .map(|p| p.repo.clone())
            .ok_or_else(|| {
                Error::refused(format!(
                    "pinned program {program} has no [programs.{program}] repo in the index"
                ))
            })?;
        one(
            ctx,
            quad,
            opts,
            log,
            work,
            &repo,
            program,
            *build,
            &mut report,
        )?;
    }
    log.say(format!(
        "every pack atpkg-index-{} pins is {} on {}",
        quad.index.index_build,
        if opts.dry_run {
            "public or planned"
        } else {
            "public"
        },
        ctx.accounts.public
    ));
    Ok(report)
}

#[expect(
    clippy::too_many_arguments,
    reason = "one pack's walk over the run's shared state"
)]
fn one(
    ctx: &Ctx,
    quad: &VerifiedQuad,
    opts: &MirrorOptions,
    log: &mut Log,
    work: &Path,
    repo: &str,
    program: &str,
    build: u64,
    report: &mut MirrorReport,
) -> Result<()> {
    let tag = upload::tag(program, build);
    let mf = pkgdoc::manifest_name(program, build);
    let sig = pkgdoc::sig_name(program, build);
    let public = ctx.public_slug(repo);
    let staging = ctx.staging_slug(repo);
    let channel = &*ctx.hosts.channel;
    let unknown = |what: &str, why: &str| {
        Error::cannot_run(format!(
            "could not read {} ({why}: DNS, TLS, a timeout or no route) — nothing about {tag} \
             is known",
            public.download_url(&tag, what)
        ))
    };

    // 1. The public copy, as a client reads it.
    let pub_dir = work.join("public").join(&tag);
    let public_mf = match channel.fetch_public(&public, &tag, &mf, &pub_dir.join(&mf), MANIFEST_CAP)
    {
        Probe::Present => true,
        Probe::Absent => false,
        Probe::Unknown(why) => return Err(unknown(&mf, &why)),
    };
    let mut public_sig = false;
    if public_mf {
        match channel.fetch_public(&public, &tag, &sig, &pub_dir.join(&sig), MANIFEST_CAP) {
            Probe::Present => public_sig = true,
            Probe::Absent => {}
            Probe::Unknown(why) => return Err(unknown(&sig, &why)),
        }
    }
    if public_mf && public_sig {
        let bytes = std::fs::read(pub_dir.join(&mf)).map_err(|e| Error::io(&mf, &e))?;
        let sig_bytes = std::fs::read(pub_dir.join(&sig)).map_err(|e| Error::io(&sig, &e))?;
        let pkg = quad.verify_pkg(&bytes, &sig_bytes).map_err(|e| {
            Error::refused(format!(
                "{public}@{tag} serves a manifest the client's verifier refuses — PKG \
                 SELF-CHECK FAILED: {e}"
            ))
        })?;
        bind(&pkg, program, build, "the public", &tag)?;
        let mut complete = true;
        for (asset, _) in release_assets(&pkg) {
            match channel.probe_public(&public, &tag, &asset) {
                Probe::Present => {}
                Probe::Absent => complete = false,
                Probe::Unknown(why) => return Err(unknown(&asset, &why)),
            }
        }
        if complete {
            log.say(format!("{public}@{tag} is public and complete — skip"));
            report.skipped.push(tag);
            return Ok(());
        }
    }

    // 2. The staging copy, whole, verified.
    log.say(format!("fetching {staging}@{tag}"));
    let staged_release = ctx
        .hosts
        .staging
        .release(&staging, &tag)
        .map_err(|e| match e {
            HostError::NotFound => Error::refused(format!(
                "{staging}@{tag} could not be downloaded, and {public} lacks it — refusing"
            )),
            e => host_error(format!("read {staging}@{tag}"), &e),
        })?;
    let dir = work.join("staging").join(&tag);
    let names: Vec<&str> = staged_release.assets.iter().map(String::as_str).collect();
    download_retried(&*ctx.hosts.staging, &staging, &tag, &names, &dir).map_err(|e| match e {
        HostError::NotFound => Error::refused(format!(
            "{staging}@{tag} could not be downloaded, and {public} lacks it — refusing"
        )),
        e => host_error(format!("download {staging}@{tag}"), &e),
    })?;
    let (expected, staged_pkg) = check_staged(quad, &dir, &staged_release.assets, program, build)?;
    if public_mf {
        let same = |name: &str| {
            std::fs::read(pub_dir.join(name)).ok() == std::fs::read(dir.join(name)).ok()
        };
        if !same(&mf) {
            return Err(Error::refused(format!(
                "{public}@{tag} serves a manifest that DIFFERS from staging's — a published \
                 pack is immutable; refusing"
            )));
        }
        if public_sig && !same(&sig) {
            return Err(Error::refused(format!(
                "{public}@{tag} serves a signature that DIFFERS from staging's — refusing"
            )));
        }
    }
    log.say(format!(
        "checked {tag} (signature, program/build bind, every release asset's sha256 vs the \
         signed manifest, no extras)"
    ));

    // 3. Publish.
    let written: Vec<String> = if public_mf {
        let mut missing = Vec::new();
        for name in &expected {
            match channel.probe_public(&public, &tag, name) {
                Probe::Present => {}
                Probe::Absent => missing.push(name.clone()),
                Probe::Unknown(why) => return Err(unknown(name, &why)),
            }
        }
        if opts.dry_run {
            log.say(format!(
                "DRY RUN — {public}@{tag} is incomplete; would upload: {}",
                missing.join(" ")
            ));
            report.planned.push((tag, missing));
            return Ok(());
        }
        let files: Vec<PathBuf> = missing.iter().map(|n| dir.join(n)).collect();
        // Never a clobber: these answered 404 a moment ago, and one appearing in the window
        // is another publisher's.
        channel
            .upload(&public, &tag, &files, Replace::Never)
            .map_err(|e| host_error(format!("upload to {public}@{tag}"), &e))?;
        log.say(format!(
            "{public}@{tag} converged — uploaded: {}",
            missing.join(" ")
        ));
        report.converged.push((tag.clone(), missing.clone()));
        missing
    } else {
        match channel.release(&public, &tag) {
            Ok(_) => {
                return Err(Error::refused(format!(
                    "{public}@{tag} exists but serves no {mf} — an orphaned draft or a \
                     half-created release; inspect it, delete it, then re-run"
                )));
            }
            Err(HostError::NotFound) => {}
            Err(e) => return Err(host_error(format!("read {public}@{tag}"), &e)),
        }
        if opts.dry_run {
            log.say(format!(
                "DRY RUN — would create {public}@{tag} with: {}",
                expected.join(" ")
            ));
            report.planned.push((tag, expected));
            return Ok(());
        }
        let files: Vec<PathBuf> = expected.iter().map(|n| dir.join(n)).collect();
        let notes = public_notes(program, &staged_pkg, &dir)?;
        channel
            .create(
                &public,
                &tag,
                &format!("atpkg {program} build {build}"),
                &notes,
                &files,
            )
            .map_err(|e| host_error(format!("create {public}@{tag}"), &e))?;
        log.say(format!("created {public}@{tag}"));
        report.created.push(tag.clone());
        expected
    };

    // 4. Confirm, as a client would fetch it.
    for name in &written {
        let answer = probe_served(channel, &public, &tag, name);
        if answer != Probe::Present {
            return Err(Error::refused(format!(
                "{} answers {answer:?} after the upload — the public repo is private, or the \
                 release is not published",
                public.download_url(&tag, name)
            )));
        }
    }
    Ok(())
}

/// `atpkg-publish mirror <quad-dir> [--dry-run]`.
///
/// # Errors
///
/// A usage error for a bad command line; a refusal when the quad does not verify or a
/// pack cannot be made public.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let mut dir: Option<PathBuf> = None;
    let mut opts = MirrorOptions::publish();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => opts.dry_run = true,
            "--quad" => dir = Some(PathBuf::from(flag_value(args, &mut i, "--quad")?)),
            other if !other.starts_with("--") && dir.is_none() => dir = Some(PathBuf::from(other)),
            other => {
                return Err(Error::usage(format!(
                    "mirror: unknown argument `{other}` (mirror <quad-dir> [--dry-run])"
                )));
            }
        }
        i += 1;
    }
    let dir = dir.ok_or_else(|| Error::usage("mirror <quad-dir> [--dry-run]"))?;
    let mut log = Log::echo("atpkg-publish mirror");
    let quad = quad::verify_dir(ctx, &dir)?;
    log.say(format!(
        "quad verified through the client chain — atpkg-index-{} signed by machine {}",
        quad.index.index_build,
        quad.index.attribution().machine_id
    ));
    if !opts.dry_run {
        let ok = std::fs::metadata(&ctx.tokens.channel).is_ok_and(|m| m.len() > 0);
        if !ok {
            return Err(Error::refused(format!(
                "publishing needs the public owner's token at {} (--dry-run plans without it)",
                ctx.shown(&ctx.tokens.channel)
            )));
        }
    }
    mirror(ctx, &quad, &opts, &mut log).map(|_| ())
}
