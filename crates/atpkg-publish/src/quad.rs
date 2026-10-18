// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The signed quad (`index.toml`, its `.sig`, and the master-signed roster pair that
//! authorized it), verified by the client's own chain in-process.
//!
//! The lanes' baseline head, an index about to be published and the quad handed to the
//! mirror are all checked the same way: `atpkg::sig::admit_roster` (under the compiled-in
//! paper master), then `authorize_index` (the roster's live machines, with the
//! `machine_id`/`roster_seq` bind), then the index's own `valid_until` (gate 2).

use std::path::{Path, PathBuf};

use atpkg::sig::{Anchor, TrustedIndex};

use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::host::{HostError, Probe, Slug};

/// The four files of one quad, in the order a release carries them.
pub const QUAD_FILES: [&str; 4] = [
    "index.toml",
    "index.toml.sig",
    "aterm-machines.toml",
    "aterm-machines.toml.sig",
];

/// The index release tag for `n`.
#[must_use]
pub fn index_tag(n: u64) -> String {
    format!("atpkg-index-{n}")
}

/// A quad on disk that the client chain accepts.
pub struct VerifiedQuad {
    /// The directory holding the four files.
    pub dir: PathBuf,
    /// The verified index (derefs to `atpkg::Index`), with the roster generation that
    /// authorized it — the authority every `pkg-*.toml` under it is verified against.
    pub index: TrustedIndex,
    /// The verified roster.
    pub roster: aterm_update_core::Roster,
}

impl VerifiedQuad {
    /// Whether every client refuses this index as stale at `now_unix`: its horizon has
    /// passed, or does not read as UTC RFC3339 (which a client counts as lapsed).
    #[must_use]
    pub fn lapsed(&self, now_unix: i64) -> bool {
        aterm_types::rfc3339::parse_utc(&self.index.valid_until)
            .is_none_or(|until| atpkg::check_freshness(now_unix, until).is_err())
    }

    /// One of its files.
    #[must_use]
    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Verify a `pkg-<program>-<build>.toml` under the roster generation that authorized
    /// this index, and parse it — the client's rule (`TrustedIndex::verify_pkg`).
    ///
    /// # Errors
    ///
    /// A refusal when no live machine of that roster signed these bytes, or the manifest
    /// does not parse.
    pub fn verify_pkg(&self, pkg: &[u8], sig: &[u8]) -> Result<atpkg::PkgManifest> {
        let verified = self.index.verify_pkg(pkg.to_vec(), sig).map_err(|why| {
            Error::refused(format!(
                "the client's verifier refuses the package manifest: {why:?}"
            ))
        })?;
        atpkg::parse_pkg(&verified).map_err(|why| {
            Error::refused(format!(
                "the package manifest verified but does not parse: {why:?}"
            ))
        })
    }
}

/// Whether a verification holds the index to its own `valid_until` (gate 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// The client's rule: a lapsed or unreadable horizon is refused. For what is about
    /// to be published, mirrored or self-checked.
    Enforce,
    /// The signature, the roster (with its own horizon) and the attribution only: for the
    /// head a publish is built over and the floor a group is judged against. Re-signing a
    /// lapsed head with a new horizon is how a channel every client refuses recovers.
    Ignore,
}

/// Verify the quad in `dir` under `ctx.anchor` at `ctx.now_unix`, freshness enforced.
///
/// # Errors
///
/// A refusal naming a missing file, an empty anchor, the client's verdict, or a lapsed
/// or unreadable `valid_until`.
pub fn verify_dir(ctx: &Ctx, dir: &Path) -> Result<VerifiedQuad> {
    verify_dir_with(ctx, dir, Freshness::Enforce)
}

/// [`verify_dir`], with the index's own horizon checked or not.
///
/// # Errors
///
/// As [`verify_dir`]; under [`Freshness::Ignore`] never for the index's horizon.
pub fn verify_dir_with(ctx: &Ctx, dir: &Path, freshness: Freshness) -> Result<VerifiedQuad> {
    let mut bytes = Vec::with_capacity(QUAD_FILES.len());
    for name in QUAD_FILES {
        let path = dir.join(name);
        bytes.push(std::fs::read(&path).map_err(|e| {
            Error::refused(format!(
                "{} is missing {name} ({e}) — a client builds ONE candidate from index.toml, \
                 its .sig and the roster pair; a release missing any of the four authorizes \
                 nothing",
                ctx.shown(dir)
            ))
        })?);
    }
    let [index, index_sig, roster, roster_sig]: [Vec<u8>; 4] = bytes
        .try_into()
        .map_err(|_| Error::cannot_run("the quad did not read as four files"))?;
    if ctx.anchor.is_empty() {
        return Err(Error::refused(
            "pins::PAPER_MASTER_PUBKEYS is EMPTY — this build trusts no paper master, so \
             nothing it signs or checks could be verified by any client built from it",
        ));
    }
    let anchor = Anchor::of(ctx.anchor.clone(), 0);
    let trusted =
        atpkg::admit_roster(&anchor, roster.clone(), &roster_sig, ctx.now_unix).map_err(|why| {
            Error::refused(format!(
                "the roster does not verify under the paper master: {why:?}"
            ))
        })?;
    let (index, _) = trusted.authorize_index(index, &index_sig).map_err(|why| {
        Error::refused(format!("the client's verifier refuses this index: {why:?}"))
    })?;
    if freshness == Freshness::Enforce {
        let valid_until = aterm_types::rfc3339::parse_utc(&index.valid_until).ok_or_else(|| {
            Error::refused(format!(
                "the index's valid_until `{}` is not UTC RFC3339 — every client reads an \
                 unparseable stamp as LAPSED",
                index.valid_until
            ))
        })?;
        atpkg::check_freshness(ctx.now_unix, valid_until).map_err(|_| {
            Error::refused(format!(
                "the index's valid_until {} has lapsed — every client refuses it as stale",
                index.valid_until
            ))
        })?;
    }
    let verified_roster = aterm_update_core::verify_roster(
        &ctx.anchor.iter().map(String::as_str).collect::<Vec<_>>(),
        roster,
        &roster_sig,
    )
    .map_err(|why| Error::refused(format!("the roster does not verify: {why:?}")))?;
    let roster = aterm_update_core::Roster::parse(&verified_roster)
        .map_err(|why| Error::refused(format!("the roster does not parse: {why:?}")))?;
    Ok(VerifiedQuad {
        dir: dir.to_path_buf(),
        index,
        roster,
    })
}

/// The newest published index number among `releases`. Drafts are skipped: the download
/// host answers 404 for one, so N+1 above it is a hole in the tag run every client walks.
#[must_use]
pub fn head_number(releases: &[crate::host::Release]) -> Option<u64> {
    releases
        .iter()
        .filter(|r| !r.draft)
        .filter_map(|r| r.tag.strip_prefix("atpkg-index-")?.parse::<u64>().ok())
        .max()
}

/// The channel head's number, `Ok(None)` when `slug` serves no published index. Listed
/// through the host when this identity reads with a token; with none (a keyless builder,
/// [`crate::host::ReleaseHost::reads_anonymously`]) walked on the anonymous download host
/// with the client's own walk (`atpkg::net::newest_index`), as every client finds it.
///
/// # Errors
///
/// Why the channel could not be read: a listing that failed ([`HostError::NotFound`] for no
/// such repo), or a HEAD that said neither.
pub fn head_build(ctx: &Ctx, slug: &Slug) -> std::result::Result<Option<u64>, HostError> {
    let host = &ctx.hosts.channel;
    if !host.reads_anonymously() {
        return host.releases(slug).map(|releases| head_number(&releases));
    }
    let mut said: Option<String> = None;
    let walked = atpkg::net::newest_index(0, &mut |n| match host.probe_public(
        slug,
        &index_tag(n),
        QUAD_FILES[0],
    ) {
        Probe::Present => Ok(true),
        Probe::Absent => Ok(false),
        Probe::Unknown(why) => {
            said = Some(why.clone());
            Err(why)
        }
    });
    match (walked, said) {
        (Ok((n, _)), _) => Ok(Some(n)),
        (Err(_), Some(why)) => Err(HostError::Unreachable(format!(
            "the download host did not say whether an index is published: {why}"
        ))),
        (Err(_), None) => Ok(None),
    }
}

/// Download `names` (of [`QUAD_FILES`]) of index release `tag` into `dir`: through the
/// host's API with a token, else from the anonymous download host under the client's caps
/// ([`head_build`]).
///
/// # Errors
///
/// [`HostError::NotFound`] for an absent asset; otherwise why the download failed.
pub fn download_quad_files(
    ctx: &Ctx,
    slug: &Slug,
    tag: &str,
    names: &[&str],
    dir: &Path,
) -> std::result::Result<(), HostError> {
    let host = &ctx.hosts.channel;
    if !host.reads_anonymously() {
        return host.download(slug, tag, names, dir);
    }
    for name in names {
        let cap = match *name {
            "index.toml" => atpkg::net::MANIFEST_CAP,
            n if n.ends_with(".sig") => atpkg::net::SIG_CAP,
            _ => atpkg::net::ROSTER_CAP,
        };
        match host.fetch_public(slug, tag, name, &dir.join(name), cap) {
            Probe::Present => {}
            Probe::Absent => return Err(HostError::NotFound),
            Probe::Unknown(why) => return Err(HostError::Unreachable(why)),
        }
    }
    Ok(())
}

/// Download the channel's head quad into `dest` and verify it, freshness enforced.
///
/// # Errors
///
/// A could-not-run when the channel cannot be read; a refusal when it serves no index or
/// its head does not verify.
pub fn fetch_head(ctx: &Ctx, slug: &Slug, dest: &Path) -> Result<(u64, VerifiedQuad)> {
    fetch_head_with(ctx, slug, dest, Freshness::Enforce)
}

/// [`fetch_head`], with the head's own horizon checked or not ([`Freshness`]). A download
/// the host does not answer is tried three times, [`crate::host::ReleaseHost::retry_pause`]
/// apart.
///
/// # Errors
///
/// As [`fetch_head`].
pub fn fetch_head_with(
    ctx: &Ctx,
    slug: &Slug,
    dest: &Path,
    freshness: Freshness,
) -> Result<(u64, VerifiedQuad)> {
    let host = &ctx.hosts.channel;
    let n = head_build(ctx, slug)
        .map_err(|e| Error::cannot_run(format!("could not list the releases of {slug}: {e}")))?
        .ok_or_else(|| {
            Error::refused(format!(
                "{slug} serves no published atpkg-index-<n> release"
            ))
        })?;
    let tag = index_tag(n);
    let mut last = HostError::NotFound;
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(host.retry_pause());
        }
        match download_quad_files(ctx, slug, &tag, &QUAD_FILES, dest) {
            Ok(()) => return Ok((n, verify_dir_with(ctx, dest, freshness)?)),
            Err(e @ HostError::Unreachable(_)) => last = e,
            Err(e) => {
                return Err(Error::refused(format!(
                    "{tag} on {slug} does not carry the four quad files ({e}) — a client \
                     finds no candidate there either"
                )));
            }
        }
    }
    Err(Error::cannot_run(format!(
        "could not download {tag} from {slug}: {last}"
    )))
}
