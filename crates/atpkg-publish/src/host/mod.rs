// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`ReleaseHost`]: every operation the producer performs on GitHub releases, as one
//! trait. The production adapter ([`gh::GhCli`]) is the only code that runs `gh`; tests
//! drive [`fake::FakeHost`], an in-memory host that can play a draft, a one-shot HTTP 500,
//! an unreachable owner and a private repo.
//!
//! Two rules are structural rather than tested:
//! * every [`ReleaseHost::create`] is a prerelease that never takes `/releases/latest`
//!   (that pointer belongs to the app release);
//! * nothing replaces a published asset except a [`Replace::Clobber`] upload, which only
//!   the staging owner's convergence and the Linux lane's manifest extension ask for.

pub mod fake;
pub mod gh;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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

    /// The unmetered, anonymous download-host URL of one release asset — what every
    /// client fetches.
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
    /// The host could not be reached or answered something else (a 500, a rate limit).
    /// Not an answer: a caller must never read it as "absent".
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
    /// Never: an asset that appeared meanwhile is another publisher's, and the upload
    /// fails loudly.
    Never,
    /// Replace: the staging owner's own partial publish converging, or the Linux lane
    /// extending a manifest it has proven is a byte-prefix extension.
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
    /// [`HostError::Unreachable`].
    fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError>;

    /// Download `assets` of release `tag` into `dir`, replacing a partial file an
    /// earlier attempt left there.
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

    /// Publish the draft `tag` (it stays a prerelease) — the step a killed create never
    /// reached.
    ///
    /// # Errors
    ///
    /// Why the edit failed.
    fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError>;

    /// What the anonymous download host answers for one asset (HEAD, no redirect
    /// followed) — how a client sees it.
    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe;

    /// GET one asset from the download host into `dest`, at most `cap` bytes (removed
    /// again unless [`Probe::Present`]; an asset over the cap is never present).
    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe;

    /// Clone `slug` into `dest` (the lanes' scratch clones of the program repos).
    ///
    /// # Errors
    ///
    /// Why the clone failed.
    fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError>;

    /// How long a caller waits before retrying an operation this host did not answer
    /// ([`HostError::Unreachable`]). Zero unless the host is a real network service: an
    /// in-memory host answers the same instantly, and a test never sleeps.
    fn retry_pause(&self) -> Duration {
        Duration::ZERO
    }

    /// Whether this identity reads with no credential: no token file, so `gh` has nothing
    /// to read the API with on a keyless builder. Then the channel head is found and
    /// fetched on the anonymous download host, as a client finds it
    /// ([`crate::quad::head_build`]).
    fn reads_anonymously(&self) -> bool {
        false
    }

    /// Bring the scratch clone `dir` of `slug` up to date as this identity: every branch
    /// into `refs/remotes/origin/*`, and the tags. A clone's own `origin` is never fetched
    /// by name, because git would authenticate with whatever credential is ambient.
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
