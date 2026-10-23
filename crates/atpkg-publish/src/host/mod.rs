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

    /// Updates the scratch clone `dir` of `slug` as this identity.
    ///
    /// # Errors
    ///
    /// Why the fetch failed.
    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError>;
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
}
