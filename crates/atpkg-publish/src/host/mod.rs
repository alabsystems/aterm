// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`ReleaseHost`] trait and types for GitHub release operations.

pub mod agh;
/// In-memory fake host for tests and debug builds.
#[cfg(any(test, debug_assertions))]
pub mod fake;
pub mod rest;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The pause between attempts at a call GitHub did not answer: its release-asset
/// endpoint's intermittent HTTP 500 is usually still answering a moment later.
pub const RETRY_PAUSE: Duration = Duration::from_secs(5);

/// `<owner>/<repo>` on the release host.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slug {
    /// The account.
    pub owner: String,
    /// The repository.
    pub repo: String,
}

impl Slug {
    /// `owner/repo`.
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
        }
    }

    /// Returns the anonymous download-host URL of a release asset.
    #[must_use]
    pub fn download_url(&self, tag: &str, asset: &str) -> String {
        format!(
            "https://github.com/{}/{}/releases/download/{tag}/{asset}",
            self.owner, self.repo
        )
    }
}

impl std::fmt::Display for Slug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// One release, as the (authenticated) API sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Its tag.
    pub tag: String,
    /// A draft: invisible to the download host and to every client.
    pub draft: bool,
    /// A prerelease.
    pub prerelease: bool,
    /// Its asset names.
    pub assets: Vec<String>,
}

/// What the anonymous download host answered for one asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// 200, or the 302 into the CDN: published.
    Present,
    /// 404: not published (absent, a draft, or a private repo).
    Absent,
    /// No answer, or one that says neither — nothing is known about the asset.
    Unknown(String),
}

/// Why a host operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    /// The release (or asset) does not exist: an answer.
    NotFound,
    /// The host could not be reached or returned a transient error.
    Unreachable(String),
    /// The host refused the operation (an asset that already exists, a bad token).
    Refused(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found"),
            Self::Unreachable(why) => write!(f, "unreachable: {why}"),
            Self::Refused(why) => write!(f, "refused: {why}"),
        }
    }
}

/// Whether an upload may replace an asset that is already there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replace {
    /// Never replace an existing asset.
    Never,
    /// Replace an existing asset when converging staging or extending a manifest.
    Clobber,
}

/// Every release operation the producer performs.
pub trait ReleaseHost {
    /// Every release of `slug`, drafts included (callers filter).
    ///
    /// # Errors
    ///
    /// [`HostError::NotFound`] for no such repo; otherwise why it failed.
    fn releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError>;

    /// The release tagged `tag`.
    ///
    /// # Errors
    ///
    /// [`HostError::NotFound`] when there is none — an answer, unlike
    /// [`HostError::Unreachable`]; otherwise why it failed.
    fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError>;

    /// Downloads `assets` of release `tag` into `dir`.
    ///
    /// # Errors
    ///
    /// [`HostError::NotFound`] when the release or an asset is absent.
    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> Result<(), HostError>;

    /// Create release `tag` carrying `files` — always a prerelease, never `latest`.
    ///
    /// # Errors
    ///
    /// [`HostError::Refused`] when it already exists (a racing publisher).
    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> Result<(), HostError>;

    /// Upload `files` to the existing release `tag`.
    ///
    /// # Errors
    ///
    /// [`HostError::Refused`] for an asset that exists under [`Replace::Never`].
    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> Result<(), HostError>;

    /// Publishes the draft release `tag` as a prerelease.
    ///
    /// # Errors
    ///
    /// Why the edit failed.
    fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError>;

    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe;

    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe;

    /// Clone `slug` into `dest` (the lanes' scratch clones of the program repos).
    ///
    /// # Errors
    ///
    /// Why the clone failed.
    fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError>;

    fn retry_pause(&self) -> Duration {
        Duration::ZERO
    }

    fn reads_anonymously(&self) -> bool {
        false
    }

    /// Whether this identity or an anonymous reader observes repository `slug`.
    /// `Ok(false)` means both repository reads answered HTTP 404. A missing repository and
    /// a private repository hidden from both readers are indistinguishable by those answers;
    /// this is a visibility result, not proof of nonexistence. A pack's upload asks it when
    /// this identity cannot list the public owner's releases, and permits private dev staging
    /// after those two 404s (the harness design's §11 D50), without publishing publicly.
    ///
    /// # Errors
    ///
    /// Why it could not be told. A host that cannot ask says so (this default), which keeps
    /// the caller's could-not-run.
    fn repo_seen(&self, slug: &Slug) -> Result<bool, HostError> {
        Err(HostError::Unreachable(format!(
            "this host cannot tell whether {slug} exists"
        )))
    }

    /// Updates the scratch clone `dir` of `slug` as this identity.
    ///
    /// # Errors
    ///
    /// Why the fetch failed.
    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError>;

    /// The published releases of `slug`, read as Read (design §10.3): no draft, and no writer
    /// needed. Default: `releases(slug)` without its drafts.
    ///
    /// # Errors
    ///
    /// As `releases`.
    fn published_releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        Ok(self
            .releases(slug)?
            .into_iter()
            .filter(|r| !r.draft)
            .collect())
    }

    /// The published release `tag` of `slug`, read as Read (`GET /releases/tags/{tag}`).
    /// Default: `release(slug, tag)`, a draft answering `NotFound`.
    ///
    /// # Errors
    ///
    /// As `release`, and [`HostError::NotFound`] for a draft.
    fn published_by_tag(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
        match self.release(slug, tag) {
            Ok(release) if release.draft => Err(HostError::NotFound),
            other => other,
        }
    }

    /// Whether a write to `slug` can be sent now: design §10.2's pre-claim check, run before a
    /// build number is claimed or anything is built (finding F2). Default: `Ok(())`.
    ///
    /// # Errors
    ///
    /// Why a write to `slug` could not be sent now.
    fn write_preflight(&self, slug: &Slug) -> Result<(), HostError> {
        let _ = slug;
        Ok(())
    }

    /// Fetches `slug`'s `refs/heads/main` from this host into the repository `dir` as the ref
    /// `into`, as this identity, writing no other ref: the main of record a pack's source is
    /// held to (D25), read from the host and never from `dir`'s own `origin`.
    ///
    /// # Errors
    ///
    /// Why the fetch failed. A host that cannot read a repository's main refuses, so a source
    /// check over it never passes.
    fn fetch_main(&self, slug: &Slug, dir: &Path, into: &str) -> Result<(), HostError> {
        let _ = (dir, into);
        Err(HostError::Refused(format!(
            "this host does not read {slug}'s main"
        )))
    }
}

/// A shared handle to a host.
pub type HostRef = Arc<dyn ReleaseHost + Send + Sync>;

/// The two identities a publish speaks as.
#[derive(Clone)]
pub struct Hosts {
    /// The private staging owner, where packs are uploaded first.
    pub staging: HostRef,
    /// The public channel owner, where the index and the packs it pins are published.
    pub channel: HostRef,
}

impl Hosts {
    /// One host serving both owners — what the tests use.
    #[must_use]
    pub fn single(host: HostRef) -> Self {
        Self {
            staging: host.clone(),
            channel: host,
        }
    }

    /// The host that serves `owner`: `staging` for `accounts.staging`, `channel` for
    /// `accounts.public` (ASCII case-insensitive), else `None` (finding F5). `staging` is
    /// asked first, so when both accounts name `owner`, as in the public build where both
    /// are `alabsystems`, it is `staging`, the host M1 read every seed through.
    #[must_use]
    pub fn for_owner(&self, accounts: &crate::ctx::Accounts, owner: &str) -> Option<&HostRef> {
        if accounts.staging.eq_ignore_ascii_case(owner) {
            Some(&self.staging)
        } else if accounts.public.eq_ignore_ascii_case(owner) {
            Some(&self.channel)
        } else {
            None
        }
    }
}

/// Which of a run's two hosts a write goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRole {
    /// `ctx.hosts.staging`: the writes to `ctx.staging_slug(..)`.
    Staging,
    /// `ctx.hosts.channel`: the writes to `ctx.index_slug()` and `ctx.public_slug(..)`.
    Channel,
}

/// Whether the host in `role` is `RestHost` under `ctx.host_choice()`: both roles under `Gh`,
/// `Channel` alone under `AghStaging`, and neither under `Agh`. A pre-claim check keeps its
/// token-file test where this holds, and calls `write_preflight` on that role's host where it
/// does not, so every M1 test that runs the lanes over `FakeHost` with the default choice sees
/// the token checks it always saw. It is keyed by the host a write goes through, never by
/// owner: in the public build both accounts are `alabsystems`, and an owner's answer would give
/// staging's writes, which go through AghHost under `AghStaging`, the channel's token test.
#[must_use]
pub fn gh_serves(ctx: &crate::ctx::Ctx, role: HostRole) -> bool {
    use crate::ctx::HostChoice;
    match ctx.host_choice() {
        HostChoice::Gh => true,
        HostChoice::AghStaging => role == HostRole::Channel,
        HostChoice::Agh => false,
    }
}

/// `host.write_preflight` of each slug in `slugs`, in order and each once: the first refusal,
/// with the slug it names. A pre-claim site asks it of every repository its run writes or
/// clones under the role's writer, because under agh both push and a release-tier grant are
/// per repository, so the index repository's answer says nothing of a program repository's.
///
/// # Errors
///
/// The first slug whose check failed, and why.
pub fn preflight_each(
    host: &dyn ReleaseHost,
    slugs: impl IntoIterator<Item = Slug>,
) -> Result<(), (Slug, HostError)> {
    let mut asked = std::collections::BTreeSet::new();
    for slug in slugs {
        if asked.insert(slug.clone()) {
            host.write_preflight(&slug).map_err(|e| (slug, e))?;
        }
    }
    Ok(())
}
