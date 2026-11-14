// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One GitHub release asset, read by name ([`Reader`]): the web download route first, then the
//! REST asset route, until one delivers or neither makes progress.
//!
//! GitHub serves a release asset on two routes to the same storage. The web route,
//! `github.com/<owner>/<repo>/releases/download/<tag>/<name>` (and `releases/latest/download/
//! <name>`, which redirects through it), is unmetered. The REST asset route reads the release
//! (`api.github.com/repos/<owner>/<repo>/releases/tags/<tag>`, or `releases/latest`) for the
//! asset's id, then `releases/assets/<id>` as `application/octet-stream`, which redirects to the
//! asset CDN. It is metered: 60 requests an hour per public address when anonymous, shared by
//! every Mac behind that address.
//!
//! Measured on 2026-11-08: on m3 (00:49–01:36, the 0.175.0 cut) the web route's tag hop answered
//! 504 to every try for over ten minutes, in windows of seconds to minutes, while the API route
//! served the same bytes; on m13 (about 03:00) the web front completed TLS and then sent no byte
//! for 30 s, while api.github.com answered at once, and once the API's release read answered 504
//! too. So a read takes the web route; a 5xx, 408 or 429, a timeout or a connection failure takes
//! the API route; a 404 on the web route is the asset's absence and asks nothing more. The read
//! keeps alternating, pausing longer each round, until a route delivers or no route has delivered
//! a byte for the reader's patience ([`PATIENCE`] unless [`Reader::patient`] says otherwise): it
//! stops on lack of progress, never on a count. A round in which neither route reached any host
//! ends at once: the link is down ([`crate::note_link_down`]), and the caller's own cadence comes
//! back to it. A listing of a repository's releases ([`Reader::releases`]) has the API route
//! alone, so a try of it that reaches no host ends it at once too, without holding the link down.
//!
//! The API leg reads each release once per reader and rests while GitHub says its allowance is
//! spent, so one reader asks the metered route for one listing plus one request per asset (a
//! release [`Reader::releases`] listed, from a page of the API's listing of a repository's
//! releases, needs no listing of its own); a caller that paces its own checks reads
//! [`Reader::api_requests`].
//!
//! The route changes where the bytes come from, never what is accepted: every caller judges
//! them (a signature, a pinned SHA-256) exactly as before. The web route and the asset CDN are
//! always anonymous. Only the API leg of a reader built with a token speaks as that token, and
//! the asset CDN never sees it (a separate, credential-less request); an updater read never
//! builds one.

use crate::github_rest::{Client, Origins, RestError, Token};
use crate::http::{handshake_in, location_header, reached_no_host};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// How long a read goes on while no route delivers a byte, unless [`Reader::patient`] says
/// otherwise.
pub const PATIENCE: Duration = Duration::from_secs(120);

/// The longest pause between two rounds.
const PAUSE_CAP: Duration = Duration::from_secs(30);

/// The connect bound of each request, in seconds (DNS, TCP and TLS).
const CONNECT_SECS: &str = "10";

/// The whole-request bound of a small read or a HEAD, in seconds: a document of at most a few
/// MiB that has not arrived by then is a stalled request, not a slow one.
const SMALL_SECS: &str = "60";

/// A file read's request ends after this many seconds under [`STALL_BYTES`] a second; the next
/// round continues from what arrived.
const STALL_SECS: &str = "30";
/// See [`STALL_SECS`].
const STALL_BYTES: &str = "4096";

/// Which release of a repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release<'a> {
    /// The repository's latest release, as GitHub names it.
    Latest,
    /// The release of this tag.
    Tag(&'a str),
}

/// One named asset of one release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asset<'a> {
    /// The repository's owner.
    pub owner: &'a str,
    /// The repository.
    pub repo: &'a str,
    /// The release.
    pub release: Release<'a>,
    /// The asset's name.
    pub name: &'a str,
}

impl<'a> Asset<'a> {
    /// Asset `name` of release `tag` in `owner/repo`.
    #[must_use]
    pub const fn tagged(owner: &'a str, repo: &'a str, tag: &'a str, name: &'a str) -> Self {
        Asset {
            owner,
            repo,
            release: Release::Tag(tag),
            name,
        }
    }

    /// Asset `name` of the latest release in `owner/repo`.
    #[must_use]
    pub const fn latest(owner: &'a str, repo: &'a str, name: &'a str) -> Self {
        Asset {
            owner,
            repo,
            release: Release::Latest,
            name,
        }
    }

    /// Asset `name` of `release` in `slug` (`owner/repo`); `None` for a slug without exactly
    /// one `/`, or any segment that is not [`crate::cdn::path_segment_safe`].
    #[must_use]
    pub fn in_slug(slug: &'a str, release: Release<'a>, name: &'a str) -> Option<Self> {
        let (owner, repo) = slug.split_once('/')?;
        let asset = Asset {
            owner,
            repo,
            release,
            name,
        };
        (!repo.contains('/') && asset.safe()).then_some(asset)
    }

    /// The asset a `https://github.com/<owner>/<repo>/releases/download/<tag>/<name>` (or
    /// `…/releases/latest/download/<name>`) URL names; `None` for any other URL, or one whose
    /// segments are not all [`crate::cdn::path_segment_safe`].
    #[must_use]
    pub fn from_web_url(url: &'a str) -> Option<Self> {
        let rest = url.strip_prefix("https://github.com/")?;
        let mut parts = rest.split('/');
        let owner = parts.next()?;
        let repo = parts.next()?;
        if parts.next()? != "releases" {
            return None;
        }
        let release = match parts.next()? {
            "download" => Release::Tag(parts.next()?),
            "latest" if parts.next()? == "download" => Release::Latest,
            _ => return None,
        };
        let name = parts.next()?;
        let asset = Asset {
            owner,
            repo,
            release,
            name,
        };
        (parts.next().is_none() && asset.safe()).then_some(asset)
    }

    /// Whether every segment is a [`crate::cdn::path_segment_safe`] one.
    fn safe(&self) -> bool {
        use crate::cdn::path_segment_safe;
        path_segment_safe(self.owner)
            && path_segment_safe(self.repo)
            && path_segment_safe(self.name)
            && match self.release {
                Release::Latest => true,
                Release::Tag(tag) => path_segment_safe(tag),
            }
    }

    /// The web route's path: `/<owner>/<repo>/releases/download/<tag>/<name>`, or
    /// `/<owner>/<repo>/releases/latest/download/<name>`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn web_path(&self) -> String {
        match self.release {
            Release::Tag(tag) => format!(
                "/{}/{}/releases/download/{tag}/{}",
                self.owner, self.repo, self.name
            ),
            Release::Latest => format!(
                "/{}/{}/releases/latest/download/{}",
                self.owner, self.repo, self.name
            ),
        }
    }

    /// The web route on github.com, as people and records name the asset.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn web_url(&self) -> String {
        format!("https://github.com{}", self.web_path())
    }
}

impl std::fmt::Display for Asset<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.release {
            Release::Tag(tag) => write!(f, "{}/{} {tag}'s {}", self.owner, self.repo, self.name),
            Release::Latest => write!(
                f,
                "{}/{}'s latest release's {}",
                self.owner, self.repo, self.name
            ),
        }
    }
}

/// The API path of `release` in `owner/repo`: `/repos/<owner>/<repo>/releases/tags/<tag>`, or
/// `/repos/<owner>/<repo>/releases/latest`.
#[cfg_attr(trust_verify, trust::skip)]
fn release_path(owner: &str, repo: &str, release: Release<'_>) -> String {
    match release {
        Release::Tag(tag) => format!("/repos/{owner}/{repo}/releases/tags/{tag}"),
        Release::Latest => format!("/repos/{owner}/{repo}/releases/latest"),
    }
}

/// Why a read ended without the asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetError {
    /// The asset is absent: the web route's 404, or the API's own fresh read of the release
    /// (no such release, or no asset of that name in it).
    Absent(String),
    /// A verdict no other try or route changes: a name that is not URL-safe, an asset over its
    /// cap, a refused redirect, a web host that refuses (403), a write here that failed, or a
    /// cancelled operation.
    Refused(String),
    /// Neither route delivered before the read stopped: each route's last failure.
    Unavailable {
        /// The web route's.
        web: String,
        /// The API route's.
        api: String,
    },
    /// The caller's stop flag ended the read ([`Reader::to_file_until`]); its partial file is
    /// gone.
    Stopped,
}

impl AssetError {
    /// Whether the asset is absent ([`AssetError::Absent`]).
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent(_))
    }
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent(why) | Self::Refused(why) => f.write_str(why),
            Self::Unavailable { web, api } => {
                write!(f, "the web route failed ({web}); the API route too ({api})")
            }
            Self::Stopped => f.write_str(crate::DOWNLOAD_STOPPED),
        }
    }
}

/// What one try on one route came to.
#[derive(Debug)]
enum Try<T> {
    /// The route delivered.
    Delivered(T),
    /// The asset is absent ([`AssetError::Absent`]).
    Absent(String),
    /// A verdict that ends the read ([`AssetError::Refused`]).
    Refused(String),
    /// This route failed this time.
    Failed {
        /// How.
        why: String,
        /// It reached no host.
        unreached: bool,
    },
}

impl<T> Try<T> {
    const fn failed(why: String) -> Try<T> {
        Try::Failed {
            why,
            unreached: false,
        }
    }

    /// The same verdict for a try that delivers something else.
    fn cast<U>(self) -> Try<U> {
        match self {
            Try::Delivered(_) => Try::failed("delivered nothing usable".into()),
            Try::Absent(why) => Try::Absent(why),
            Try::Refused(why) => Try::Refused(why),
            Try::Failed { why, unreached } => Try::Failed { why, unreached },
        }
    }
}

/// One release as the API's releases listing names it ([`Reader::releases`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedRelease {
    /// Its tag.
    pub tag: String,
    /// Whether it is a draft (listed only to a token that may push).
    pub draft: bool,
    /// Whether it is a prerelease.
    pub prerelease: bool,
    /// Its assets' names, as listed.
    pub assets: Vec<String>,
}

/// The most releases one [`Reader::releases`] lists: GitHub's largest page.
pub const RELEASES_PAGE_MAX: u8 = 100;

/// A release as the API's releases listing gives it, the fields read here. Its flags are
/// required: a listing that does not say whether a release is a draft names none.
#[derive(serde::Deserialize)]
struct ListedJson {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<ListedAssetJson>,
}

/// An asset of a [`ListedJson`].
#[derive(serde::Deserialize)]
struct ListedAssetJson {
    name: String,
    id: u64,
}

/// A release as the API listed it.
#[derive(Clone)]
struct Listing {
    /// Its [`release_path`]s: the one it was asked by, and its tag's.
    paths: Vec<String>,
    /// Its tag.
    tag: String,
    /// Each asset's name and id.
    assets: Vec<(String, u64)>,
}

impl Listing {
    fn id_of(&self, name: &str) -> Option<u64> {
        self.assets
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }
}

/// What one reader knows between its tries.
#[derive(Default)]
struct ApiState {
    /// Releases already read.
    listings: Vec<Listing>,
    /// Releases this reader has claimed from its [`Reader::api_releases`] allowance: each before
    /// its listing is asked for, none given back when that read fails.
    claimed: u32,
    /// The API refused for its rate limit: no API request before this instant, and why.
    resting: Option<(Instant, String)>,
    /// Since when no try of this reader, on either route and in any of its reads, has delivered
    /// or moved a byte; `None` once one has.
    quiet_since: Option<Instant>,
}

/// Reads release assets: [`Reader::bytes`], [`Reader::to_file`], [`Reader::to_file_until`],
/// [`Reader::latest_tag`], and lists releases: [`Reader::releases`].
///
/// Its patience is the reader's, shared by every read it serves, at once or one after another:
/// once no route has delivered a byte for the patience, each later read tries each route once
/// until one delivers again. So one pass that reads many assets through one reader waits out an
/// outage once, not once per asset; make one reader per pass.
pub struct Reader {
    /// The web origin (`https://github.com`).
    web: String,
    /// Whether every origin is https, so curl may be told to follow nothing else.
    https: bool,
    /// The API leg.
    api: Client,
    /// How long the reader goes on without progress.
    patience: Duration,
    /// The first pause; each later one doubles, up to [`PAUSE_CAP`].
    pause: Duration,
    /// What the reader knows between tries.
    state: std::sync::Mutex<ApiState>,
    /// Held while a release is read from the API, so reads of one release that run at once
    /// ask for its listing once.
    listing_gate: std::sync::Mutex<()>,
    /// The API requests this reader made.
    api_requests: AtomicU32,
    /// How many releases the API route may list for this reader ([`Reader::api_releases`]);
    /// `None` for no bound.
    api_releases: Option<u32>,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("web", &self.web)
            .field("api", &self.api)
            .field("patience", &self.patience)
            .finish_non_exhaustive()
    }
}

impl Reader {
    /// An anonymous reader of github.com: what every updater and package read uses.
    #[must_use]
    pub fn anonymous() -> Reader {
        Reader::new(Origins::github(), None)
    }

    /// A reader of `origins` whose API leg speaks as `token`, or anonymously. A tool that holds a
    /// token passes it so its fallback draws on the token's own allowance instead of the
    /// address's shared anonymous one; the web route and the asset CDN stay anonymous.
    #[must_use]
    pub fn new(origins: Origins, token: Option<Token>) -> Reader {
        let https = [&origins.api, &origins.web]
            .into_iter()
            .chain(&origins.cdn)
            .all(|origin| origin.starts_with("https://"));
        Reader {
            web: origins.web.clone(),
            https,
            api: Client::new(origins, token),
            patience: PATIENCE,
            pause: Duration::from_secs(1),
            state: std::sync::Mutex::new(ApiState::default()),
            listing_gate: std::sync::Mutex::new(()),
            api_requests: AtomicU32::new(0),
            api_releases: None,
        }
    }

    /// The same reader, going on for `patience` without progress before it stops.
    #[must_use]
    pub const fn patient(mut self, patience: Duration) -> Reader {
        self.patience = patience;
        self
    }

    /// The same reader, whose API route lists at most `releases` releases: a read of a release
    /// it already listed costs that release's assets alone, and a read of another once the
    /// allowance is spent tries the web route only. A client that reads many releases in one
    /// pass bounds by this what an outage of the download host costs the anonymous allowance
    /// its address shares (60 requests an hour).
    #[must_use]
    pub const fn api_releases(mut self, releases: u32) -> Reader {
        self.api_releases = Some(releases);
        self
    }

    /// A reader of a loopback fixture's origins with a millisecond first pause, for tests.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn loopback(origins: Origins, token: Option<Token>, patience: Duration) -> Reader {
        let mut reader = Reader::new(origins, token).patient(patience);
        reader.pause = Duration::from_millis(1);
        reader
    }

    /// The API requests this reader has made so far.
    #[must_use]
    pub fn api_requests(&self) -> u32 {
        self.api_requests.load(Ordering::Relaxed)
    }

    /// The bytes of `asset`, at most `cap` of them, each try from the first byte.
    /// # Errors
    /// [`AssetError`]: absent, refused, or unavailable on both routes.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn bytes(&self, asset: &Asset<'_>, cap: u64) -> Result<Vec<u8>, AssetError> {
        let scratch = crate::http::VendorScratch::labelled("asset").map_err(AssetError::Refused)?;
        let body = scratch.0.join("body");
        self.read(asset, cap, &body, Shape::Small, None)?;
        read_capped(&body, cap)
    }

    /// The body at `url`, at most `cap` bytes: a github.com release download URL
    /// ([`Asset::from_web_url`]) is read as that asset ([`Reader::bytes`], on either route), and
    /// any other https URL from its own host, anonymously ([`crate::vendor_get`]).
    /// # Errors
    /// As [`Reader::bytes`] for a release URL; for any other, [`AssetError::Absent`] on its
    /// host's 404 and [`AssetError::Refused`] for every other failure.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn bytes_at(&self, url: &str, cap: u64) -> Result<Vec<u8>, AssetError> {
        if let Some(asset) = Asset::from_web_url(url) {
            return self.bytes(&asset, cap);
        }
        match crate::vendor_get(url, cap, None) {
            Ok(crate::VendorResponse::Body { bytes, .. }) => Ok(bytes),
            Ok(crate::VendorResponse::NotModified { .. }) => Err(AssetError::Refused(format!(
                "{url} answered 304 to an unconditional read"
            ))),
            Err(crate::HttpError::VendorStatus { code: 404, url }) => {
                Err(AssetError::Absent(format!("HTTP 404 for {url}")))
            }
            Err(other) => Err(AssetError::Refused(other.to_string())),
        }
    }

    /// `asset` into `dest`, at most `cap` bytes: through `<dest>.part`, which a tagged release's
    /// later try (and a later call) continues and which is renamed onto `dest` once complete.
    /// The latest release's file starts over at each try, since the latest may move between
    /// them.
    /// # Errors
    /// [`AssetError`]: absent, refused, or unavailable on both routes.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn to_file(&self, asset: &Asset<'_>, dest: &Path, cap: u64) -> Result<(), AssetError> {
        self.file(asset, dest, cap, None)
    }

    /// The body at `url` into `dest`, at most `cap` bytes: a github.com release download URL
    /// ([`Asset::from_web_url`]) is read as that asset ([`Reader::to_file`], on either route),
    /// and any other https URL from its own host, anonymously ([`crate::vendor_download_to`]);
    /// either way through `<dest>.part`.
    /// # Errors
    /// As [`Reader::to_file`] for a release URL; for any other, [`AssetError::Absent`] on its
    /// host's 404 and [`AssetError::Refused`] for every other failure.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn to_file_at(&self, url: &str, dest: &Path, cap: u64) -> Result<(), AssetError> {
        if let Some(asset) = Asset::from_web_url(url) {
            return self.to_file(&asset, dest, cap);
        }
        crate::vendor_download_to(url, dest, cap).map_err(|error| match error {
            crate::HttpError::VendorStatus { code: 404, url } => {
                AssetError::Absent(format!("HTTP 404 for {url}"))
            }
            other => AssetError::Refused(other.to_string()),
        })
    }

    /// [`Reader::to_file`] that gives up as soon as `stop` is set: the transfer in flight is
    /// ended, the partial file removed, and the call answers [`AssetError::Stopped`]. A transfer
    /// that already completed stands.
    /// # Errors
    /// As [`Reader::to_file`], and [`AssetError::Stopped`].
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn to_file_until(
        &self,
        asset: &Asset<'_>,
        dest: &Path,
        cap: u64,
        stop: &AtomicBool,
    ) -> Result<(), AssetError> {
        self.file(asset, dest, cap, Some(stop))
    }

    #[cfg_attr(trust_verify, trust::skip)]
    fn file(
        &self,
        asset: &Asset<'_>,
        dest: &Path,
        cap: u64,
        stop: Option<&AtomicBool>,
    ) -> Result<(), AssetError> {
        let part = part_path(dest)
            .ok_or_else(|| AssetError::Refused(format!("{} has no file name", dest.display())))?;
        let shape = match asset.release {
            Release::Tag(_) => Shape::Resumable,
            Release::Latest => Shape::Whole,
        };
        match self.read(asset, cap, &part, shape, stop) {
            Ok(()) => {}
            Err(AssetError::Stopped) => {
                let _ = std::fs::remove_file(&part);
                return Err(AssetError::Stopped);
            }
            Err(e) => return Err(e),
        }
        std::fs::rename(&part, dest).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            AssetError::Refused(format!("finalize {}: {e}", dest.display()))
        })
    }

    /// The tag of `owner/repo`'s latest release: the web route's unmetered redirect
    /// (`releases/latest/download/<name>`, not followed, which names the tag whether or not that
    /// release carries `name`), else the API's latest release. A later read of that tag through
    /// the API route reuses the listing; whether the release carries `name` is that read's
    /// answer.
    /// # Errors
    /// [`AssetError`]: absent (no latest release), refused (a redirect to anything but this
    /// repository's tag route for `name`), or unavailable on both routes.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn latest_tag(&self, owner: &str, repo: &str, name: &str) -> Result<String, AssetError> {
        let asset = Asset::latest(owner, repo, name);
        if !asset.safe() {
            return Err(AssetError::Refused(format!(
                "{asset} is not a URL-safe release asset name"
            )));
        }
        let url = format!("{}{}", self.web, asset.web_path());
        self.rounds(
            None,
            &|| 0,
            Some(&mut || self.head_tag(&url, &asset)),
            &mut || match self.listing(&asset) {
                Ok(listing) => Try::Delivered(listing.tag),
                Err(leg) => leg.cast(),
            },
        )
    }

    /// Page `page` (from 1) of `owner/repo`'s releases in pages of `count` (1 to
    /// [`RELEASES_PAGE_MAX`]): the API's listing, which names them newest first by creation, each
    /// with its tag, its draft and prerelease flags and its assets' names. A page shorter than
    /// `count` is the listing's last. Only the API lists releases, so this is one metered request
    /// per try ([`Reader::api_requests`]), tried with the reader's patience as any API try is,
    /// and a try that reaches no host ends it at once; it claims nothing from
    /// [`Reader::api_releases`]. GitHub lists a draft only to a token that may push. Each
    /// published release listed is remembered, so a later read of its assets through the API
    /// route asks for no listing of its own.
    /// # Errors
    /// [`AssetError`]: absent (no such repository), refused (names that are not URL-safe, page 0,
    /// or a page longer than asked for), or unavailable.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn releases(
        &self,
        owner: &str,
        repo: &str,
        count: u8,
        page: u32,
    ) -> Result<Vec<ListedRelease>, AssetError> {
        use crate::cdn::path_segment_safe;
        if !(path_segment_safe(owner) && path_segment_safe(repo)) {
            return Err(AssetError::Refused(format!(
                "{owner}/{repo} is not a URL-safe repository name"
            )));
        }
        if page == 0 {
            return Err(AssetError::Refused(format!(
                "{owner}/{repo}: the API numbers listing pages from 1"
            )));
        }
        let count = count.clamp(1, RELEASES_PAGE_MAX);
        let path = format!("/repos/{owner}/{repo}/releases?per_page={count}&page={page}");
        // The web route has no listing: the API's tries are the only ones.
        self.rounds(None, &|| 0, None, &mut || {
            self.list_releases(owner, repo, &path, count)
        })
    }

    /// One try of [`Reader::releases`]: the API's page at `path`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn list_releases(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        count: u8,
    ) -> Try<Vec<ListedRelease>> {
        if let Some(why) = self.resting() {
            return Try::failed(why);
        }
        self.api_requests.fetch_add(1, Ordering::Relaxed);
        let answer = match self.api.get(path) {
            Ok(answer) => answer,
            Err(e) => return self.api_failure(&e).cast(),
        };
        match answer.status {
            200 => {}
            404 => {
                return Try::Absent(format!("{owner}/{repo}: the API has no such repository"));
            }
            status => {
                return self
                    .api_failure(&RestError::Status {
                        status,
                        retry_after_secs: answer.retry_after_secs(),
                        message: rate_limit_note(&answer).unwrap_or_else(|| answer.message()),
                    })
                    .cast();
            }
        }
        let listed: Vec<ListedJson> = match aterm_json::from_slice(&answer.body) {
            Ok(listed) => listed,
            Err(e) => {
                return Try::failed(format!(
                    "{owner}/{repo}: the API's releases listing is not the JSON it documents ({e})"
                ));
            }
        };
        if listed.len() > usize::from(count) {
            return Try::Refused(format!(
                "{owner}/{repo}: the API listed {} releases for a page of {count}",
                listed.len()
            ));
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut releases = Vec::with_capacity(listed.len());
        for release in listed {
            if !release.draft && crate::cdn::path_segment_safe(&release.tag_name) {
                let tag_path = release_path(owner, repo, Release::Tag(&release.tag_name));
                if !state.listings.iter().any(|l| l.paths.contains(&tag_path)) {
                    state.listings.push(Listing {
                        paths: vec![tag_path],
                        tag: release.tag_name.clone(),
                        assets: release
                            .assets
                            .iter()
                            .map(|a| (a.name.clone(), a.id))
                            .collect(),
                    });
                }
            }
            releases.push(ListedRelease {
                tag: release.tag_name,
                draft: release.draft,
                prerelease: release.prerelease,
                assets: release.assets.into_iter().map(|a| a.name).collect(),
            });
        }
        Try::Delivered(releases)
    }

    /// The rounds of one read into `sink`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn read(
        &self,
        asset: &Asset<'_>,
        cap: u64,
        sink: &Path,
        shape: Shape,
        stop: Option<&AtomicBool>,
    ) -> Result<(), AssetError> {
        if !asset.safe() {
            return Err(AssetError::Refused(format!(
                "{asset} is not a URL-safe release asset name"
            )));
        }
        let url = format!("{}{}", self.web, asset.web_path());
        let moved = || match shape {
            Shape::Resumable => sink_len(sink),
            Shape::Small | Shape::Whole => 0,
        };
        self.rounds(
            stop,
            &moved,
            Some(&mut || self.transfer(&url, cap, sink, shape, Route::Web, stop)),
            &mut || self.api_transfer(asset, cap, sink, shape, stop),
        )
    }

    /// Alternate `web` (when the read has a web route) and `api` tries until one delivers, either
    /// ends the read, or no try of this reader has delivered or moved `moved` for the patience. A
    /// try that moved it starts the next round at once. A round in which every route tried
    /// reached no host ends at once; only one in which both routes did holds the link down
    /// ([`crate::note_link_down`]), since a machine that reaches the web route can be cut off
    /// from the API host alone.
    #[cfg_attr(trust_verify, trust::skip)]
    fn rounds<T>(
        &self,
        stop: Option<&AtomicBool>,
        moved: &dyn Fn() -> u64,
        mut web: Option<&mut dyn FnMut() -> Try<T>>,
        api: &mut dyn FnMut() -> Try<T>,
    ) -> Result<T, AssetError> {
        let mut pause = self.pause;
        loop {
            halt(stop)?;
            let web_failed = match web.as_mut() {
                Some(web) => {
                    let before = moved();
                    match self.judged(web()) {
                        Ok(t) => return t,
                        Err(_) if moved() > before => {
                            // The web route delivered bytes before it failed: it works, so
                            // continue there.
                            self.heard();
                            pause = self.pause;
                            continue;
                        }
                        Err(failed) => Some(failed),
                    }
                }
                None => None,
            };
            halt(stop)?;
            let before = moved();
            let api_failed = match self.judged(api()) {
                Ok(t) => return t,
                Err(failed) => failed,
            };
            if moved() > before {
                self.heard();
                pause = self.pause;
                continue;
            }
            let web_unreached = web_failed.as_ref().map(|failed| failed.1);
            let unavailable = AssetError::Unavailable {
                web: web_failed.map_or_else(|| "the web route lists no releases".into(), |f| f.0),
                api: api_failed.0,
            };
            if api_failed.1 {
                match web_unreached {
                    Some(true) => {
                        crate::note_link_down();
                        return Err(unavailable);
                    }
                    // The only route reached no host: the read ends, the web host unjudged.
                    None => return Err(unavailable),
                    Some(false) => {}
                }
            }
            let left = self.patience.saturating_sub(self.quiet_for());
            if left.is_zero() {
                return Err(unavailable);
            }
            wait(pause.min(left), stop)?;
            pause = (pause * 2).min(PAUSE_CAP);
        }
    }

    /// A try's outcome for the read: `Ok` ends it, `Err` is the route's failure and whether it
    /// reached no host. A route that delivered, or answered that the asset is absent, has been
    /// heard from.
    fn judged<T>(&self, tried: Try<T>) -> Result<Result<T, AssetError>, (String, bool)> {
        match tried {
            Try::Delivered(t) => {
                self.heard();
                Ok(Ok(t))
            }
            Try::Absent(why) => {
                self.heard();
                Ok(Err(AssetError::Absent(why)))
            }
            Try::Refused(why) => Ok(Err(AssetError::Refused(why))),
            Try::Failed { why, unreached } => Err((why, unreached)),
        }
    }

    /// A route delivered or answered: the reader's quiet ends.
    fn heard(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .quiet_since = None;
    }

    /// How long no try of this reader has delivered, counting from now if one just failed for
    /// the first time since one did.
    fn quiet_for(&self) -> Duration {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .quiet_since
            .get_or_insert_with(Instant::now)
            .elapsed()
    }

    /// One try of the API route: the release's asset id (the release read once per reader), the
    /// asset's CDN location, then the bytes from there.
    #[cfg_attr(trust_verify, trust::skip)]
    fn api_transfer(
        &self,
        asset: &Asset<'_>,
        cap: u64,
        sink: &Path,
        shape: Shape,
        stop: Option<&AtomicBool>,
    ) -> Try<()> {
        let id = match self.listing(asset) {
            Ok(listing) => match listing.id_of(asset.name) {
                Some(id) => id,
                None => return Try::Absent(not_listed(asset)),
            },
            Err(leg) => return leg,
        };
        if let Some(why) = self.resting() {
            return Try::failed(why);
        }
        let path = format!("/repos/{}/{}/releases/assets/{id}", asset.owner, asset.repo);
        self.api_requests.fetch_add(1, Ordering::Relaxed);
        match self.api.asset_location(&path) {
            Ok(location) => self.transfer(&location, cap, sink, shape, Route::Cdn, stop),
            Err(RestError::Redirect(why)) => Try::Refused(why),
            Err(e) => self.api_failure(&e),
        }
    }

    /// The API's rest, while it lasts.
    fn resting(&self) -> Option<String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &state.resting {
            Some((until, why)) if Instant::now() < *until => Some(why.clone()),
            Some(_) => {
                state.resting = None;
                None
            }
            None => None,
        }
    }

    /// `asset`'s release as the API lists it: remembered from an earlier read by this reader,
    /// else read now.
    #[cfg_attr(trust_verify, trust::skip)]
    fn listing(&self, asset: &Asset<'_>) -> Result<Listing, Try<()>> {
        let path = release_path(asset.owner, asset.repo, asset.release);
        let known = |state: &ApiState| {
            state
                .listings
                .iter()
                .find(|l| l.paths.contains(&path))
                .cloned()
        };
        let held = |reader: &Reader| {
            known(
                &reader
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        };
        if let Some(listing) = held(self) {
            return Ok(listing);
        }
        let _gate = self
            .listing_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(listing) = held(self) {
            return Ok(listing);
        }
        if let Some(why) = self.resting() {
            return Err(Try::failed(why));
        }
        if let Some(allowance) = self.api_releases {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.claimed >= allowance {
                return Err(Try::failed(format!(
                    "this reader's API allowance of {allowance} release(s) is spent"
                )));
            }
            state.claimed += 1;
        }
        self.api_requests.fetch_add(1, Ordering::Relaxed);
        let answer = match self.api.get(&path) {
            Ok(answer) => answer,
            Err(e) => return Err(self.api_failure(&e)),
        };
        match answer.status {
            200 => {}
            404 => {
                return Err(Try::Absent(format!(
                    "{asset}: the API has no such release in {}/{}",
                    asset.owner, asset.repo
                )));
            }
            status => {
                return Err(self.api_failure(&RestError::Status {
                    status,
                    retry_after_secs: answer.retry_after_secs(),
                    message: rate_limit_note(&answer).unwrap_or_else(|| answer.message()),
                }));
            }
        }
        #[derive(serde::Deserialize)]
        struct Listed {
            tag_name: String,
            assets: Vec<ListedAsset>,
        }
        #[derive(serde::Deserialize)]
        struct ListedAsset {
            name: String,
            id: u64,
        }
        let listed: Listed = aterm_json::from_slice(&answer.body).map_err(|e| {
            Try::failed(format!(
                "{asset}: the API's release is not the JSON it documents ({e})"
            ))
        })?;
        if !crate::cdn::path_segment_safe(&listed.tag_name) {
            return Err(Try::Refused(format!(
                "{asset}: the API named a release tag that is not URL-safe"
            )));
        }
        let mut paths = vec![path];
        let tag_path = release_path(
            asset.owner,
            asset.repo,
            Release::Tag(listed.tag_name.as_str()),
        );
        if !paths.contains(&tag_path) {
            paths.push(tag_path);
        }
        let listing = Listing {
            paths,
            tag: listed.tag_name,
            assets: listed.assets.into_iter().map(|a| (a.name, a.id)).collect(),
        };
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .listings
            .push(listing.clone());
        Ok(listing)
    }

    /// An API failure as a [`Try`]; a rate limit rests the API leg until GitHub's reset.
    #[cfg_attr(trust_verify, trust::skip)]
    fn api_failure(&self, error: &RestError) -> Try<()> {
        if let RestError::Status {
            status, message, ..
        } = error
            && let Some(rest) = rest_for(*status, message)
        {
            let why = format!("HTTP {status}: {message}");
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .resting = Some((Instant::now() + rest, why.clone()));
            return Try::failed(why);
        }
        match error {
            RestError::Malformed(why) if why.contains("cap") => Try::Refused(why.clone()),
            // curl's exit 5, 6 or 7: no proxy or host resolved, or none took the connection.
            RestError::Transport { exit, message, .. } => Try::Failed {
                why: message.clone(),
                unreached: matches!(exit, Some(5..=7)),
            },
            other => Try::failed(other.to_string()),
        }
    }

    /// One redirect-refusing HEAD of the web route's `latest` URL: the tag its `Location` names.
    #[cfg_attr(trust_verify, trust::skip)]
    fn head_tag(&self, url: &str, asset: &Asset<'_>) -> Try<String> {
        let args = vec![
            "-sS",
            "--globoff",
            "-I",
            "--max-redirs",
            "0",
            "--connect-timeout",
            CONNECT_SECS,
            "--max-time",
            SMALL_SECS,
            "-w",
            "\n%{http_code} %{time_appconnect}",
        ];
        let out = match self.run(&args, url, None) {
            Ok(out) => out,
            Err(t) => return t.cast(),
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let (headers, trailer) = match text.rfind('\n') {
            Some(i) => (&text[..i], text[i + 1..].trim()),
            None => ("", text.trim()),
        };
        let status = status_of(trailer);
        let stderr = String::from_utf8_lossy(&out.stderr);
        match status {
            301 | 302 | 303 | 307 | 308 => {
                let prefix = format!(
                    "{}/{}/{}/releases/download/",
                    self.web, asset.owner, asset.repo
                );
                let suffix = format!("/{}", asset.name);
                match location_header(headers).as_deref().and_then(|l| {
                    l.strip_prefix(prefix.as_str())?
                        .strip_suffix(suffix.as_str())
                        .filter(|tag| crate::cdn::path_segment_safe(tag))
                        .map(str::to_string)
                }) {
                    Some(tag) => Try::Delivered(tag),
                    None => Try::Refused(format!(
                        "{url}: the latest-release redirect does not name this repository's \
                         {} by a URL-safe tag",
                        asset.name
                    )),
                }
            }
            404 => Try::Absent(format!("HTTP 404 for {url}: no latest release")),
            0 => Try::Failed {
                why: no_answer(url, out.status.code(), stderr.trim()),
                unreached: reached_no_host(out.status.code(), handshake_in(trailer)),
            },
            408 | 429 | 500..=599 => Try::failed(format!("{url}: HTTP {status}")),
            other => Try::Refused(format!("{url}: HTTP {other}, not a release redirect")),
        }
    }

    /// One curl transfer of `url` into `sink`, judged by the HTTP status it reports.
    #[cfg_attr(trust_verify, trust::skip)]
    fn transfer(
        &self,
        url: &str,
        cap: u64,
        sink: &Path,
        shape: Shape,
        route: Route,
        stop: Option<&AtomicBool>,
    ) -> Try<()> {
        let Some(sink_arg) = sink.to_str() else {
            return Try::Refused(format!("{} is not UTF-8", sink.display()));
        };
        let mut offset = match shape {
            Shape::Resumable => sink_len(sink),
            Shape::Small | Shape::Whole => 0,
        };
        if offset >= cap {
            offset = 0;
        }
        if offset == 0 {
            let _ = std::fs::remove_file(sink);
        }
        let remaining = (cap - offset).to_string();
        let offset_arg = offset.to_string();
        let mut args: Vec<&str> = vec![
            "-sS",
            "--globoff",
            "-f",
            "-L",
            "--max-redirs",
            route.max_redirs(),
            "--connect-timeout",
            CONNECT_SECS,
            "--max-filesize",
            &remaining,
            "-H",
            "Accept: application/octet-stream",
            "-o",
            sink_arg,
            "-w",
            "%{http_code} %{time_appconnect}",
        ];
        if shape == Shape::Small {
            args.extend(["--max-time", SMALL_SECS]);
        } else {
            args.extend(["--speed-limit", STALL_BYTES, "--speed-time", STALL_SECS]);
        }
        if offset > 0 {
            args.extend(["--continue-at", &offset_arg]);
        }
        // The URL is the reader's own origin's, or a CDN location the API leg admitted; what
        // a redirect leads to is bound here.
        if self.https {
            args.extend(["--proto-redir", "=https"]);
        }
        let out = match self.run(&args, url, stop) {
            Ok(out) => out,
            Err(t) => return t,
        };
        let trailer = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let shown = shown(url, route);
        let verdict = match judge(
            out.status.code(),
            &trailer,
            stderr.trim(),
            route,
            &shown,
            cap,
        ) {
            Judged::Try(t) => t,
            Judged::RangeRefused => {
                let _ = std::fs::remove_file(sink);
                return Try::failed(format!(
                    "{shown}: the host would not continue the partial file; the next try \
                     starts over"
                ));
            }
        };
        if !matches!(verdict, Try::Delivered(())) && shape != Shape::Resumable {
            let _ = std::fs::remove_file(sink);
        }
        verdict
    }

    /// Run one anonymous curl of `url` with `args`, ended early once `stop` is set.
    #[cfg_attr(trust_verify, trust::skip)]
    fn run(
        &self,
        args: &[&str],
        url: &str,
        stop: Option<&AtomicBool>,
    ) -> Result<std::process::Output, Try<()>> {
        let mut command = crate::github_rest::curl(args, url);
        command
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped());
        let ran = match stop {
            Some(stop) => {
                crate::operation::run_command_until(&mut command, || stop.load(Ordering::Acquire))
            }
            None => crate::run_operation_command(&mut command),
        };
        ran.map_err(|e| {
            if e.kind() == std::io::ErrorKind::Interrupted {
                Try::failed(format!("curl ended: {e}"))
            } else {
                Try::failed(format!("cannot start curl: {e}"))
            }
        })
    }
}

/// Stop here when the caller's flag or the process's operation says so.
fn halt(stop: Option<&AtomicBool>) -> Result<(), AssetError> {
    if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
        return Err(AssetError::Stopped);
    }
    crate::check_operation().map_err(|e| AssetError::Refused(e.to_string()))
}

/// Pause for `pause`, ending early when [`halt`] would.
fn wait(pause: Duration, stop: Option<&AtomicBool>) -> Result<(), AssetError> {
    const SLICE: Duration = Duration::from_millis(50);
    let until = Instant::now() + pause;
    loop {
        halt(stop)?;
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        std::thread::sleep(left.min(SLICE));
    }
}

/// How a read writes its sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// A small document: whole each try, under a whole-request bound.
    Small,
    /// A file that starts over at each try (the latest release may move between tries).
    Whole,
    /// A file that continues from what earlier tries delivered.
    Resumable,
}

/// Which host a transfer talks to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// The web route, whose first hop redirects to the CDN (twice for `latest`).
    Web,
    /// The asset CDN, at a location the API gave.
    Cdn,
}

impl Route {
    const fn max_redirs(self) -> &'static str {
        match self {
            Self::Web => "4",
            Self::Cdn => "0",
        }
    }
}

/// [`judge`]'s answer.
#[derive(Debug)]
enum Judged {
    Try(Try<()>),
    /// The host refused to continue the partial file (416, or curl's 33).
    RangeRefused,
}

/// The status a `-w` trailer (`%{http_code} %{time_appconnect}`) reports; 0 for none.
fn status_of(trailer: &str) -> u16 {
    trailer
        .split_whitespace()
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0)
}

/// A request that got no HTTP answer, in words.
#[cfg_attr(trust_verify, trust::skip)]
fn no_answer(url: &str, exit: Option<i32>, stderr: &str) -> String {
    format!(
        "{url}: no answer ({}): {stderr}",
        exit.map_or_else(|| "ended by a signal".to_string(), |c| format!("exit {c}"))
    )
}

/// What one transfer's exit, `-w` trailer (`%{http_code} %{time_appconnect}`) and stderr mean.
/// The HTTP status decides, never curl's exit code alone: `/usr/bin/curl` 8.7.1 with `-f` over
/// HTTP/2 reports a 5xx as exit 56, as it does a connection that died mid-body.
#[cfg_attr(trust_verify, trust::skip)]
fn judge(
    exit: Option<i32>,
    trailer: &str,
    stderr: &str,
    route: Route,
    url: &str,
    cap: u64,
) -> Judged {
    let status = status_of(trailer);
    if exit == Some(0) && (200..300).contains(&status) {
        return Judged::Try(Try::Delivered(()));
    }
    if exit == Some(63) || stderr.contains("Maximum file size exceeded") {
        return Judged::Try(Try::Refused(format!(
            "{url}: the asset exceeds its {cap}-byte cap"
        )));
    }
    if crate::download_error_is_local_write(stderr) {
        return Judged::Try(Try::Refused(format!(
            "{url}: this machine could not store the bytes ({stderr})"
        )));
    }
    if exit == Some(33) || status == 416 {
        return Judged::RangeRefused;
    }
    let said = if stderr.is_empty() {
        format!("{url}: HTTP {status}")
    } else {
        format!("{url}: HTTP {status} ({stderr})")
    };
    Judged::Try(match (status, route) {
        (404, Route::Web) => Try::Absent(format!(
            "HTTP 404: the release does not carry this asset (unpublished, draft, or private): \
             {url}"
        )),
        (403, Route::Web) => Try::Refused(format!(
            "HTTP 403 from the release download host (a blocked host or proxy, not GitHub's \
             rate limit): {url}"
        )),
        (0, _) => Try::Failed {
            why: no_answer(url, exit, stderr),
            unreached: reached_no_host(exit, handshake_in(trailer)),
        },
        (_, Route::Web) if exit == Some(47) => Try::Refused(format!(
            "{url}: redirected more often than a release asset is"
        )),
        (408 | 429 | 500..=599 | 200..=299, _) | (_, Route::Cdn) => Try::failed(said),
        _ => Try::Refused(said),
    })
}

/// `url` as an error names it: a CDN location's signature is never shown.
fn shown(url: &str, route: Route) -> String {
    match route {
        Route::Web => url.to_string(),
        Route::Cdn => url.split(['?', '#']).next().unwrap_or(url).to_string(),
    }
}

/// How long a rate-limited API answer rests the API leg, or `None` when `status` and
/// `message` are not a rate limit.
fn rest_for(status: u16, message: &str) -> Option<Duration> {
    let limited = status == 429 || (status == 403 && message.contains("rate limit"));
    limited.then(|| {
        message
            .split_once("resets in ")
            .and_then(|(_, rest)| rest.split_once(" s").map(|(n, _)| n))
            .and_then(|n| n.parse::<u64>().ok())
            .map_or(Duration::from_secs(60), Duration::from_secs)
    })
}

/// A rate-limited answer's own words with its reset, for [`rest_for`]: `None` for any other.
#[cfg_attr(trust_verify, trust::skip)]
fn rate_limit_note(answer: &crate::github_rest::Answer) -> Option<String> {
    let exhausted = answer.header("x-ratelimit-remaining") == Some("0");
    let retry_after = answer
        .header("retry-after")
        .and_then(|s| s.trim().parse::<u64>().ok());
    if !(answer.status == 429 || (answer.status == 403 && (exhausted || retry_after.is_some()))) {
        return None;
    }
    let wait = retry_after.or_else(|| {
        let reset = answer
            .header("x-ratelimit-reset")
            .and_then(|s| s.trim().parse::<u64>().ok())?;
        Some(reset.saturating_sub(aterm_time::unix_secs()?))
    });
    Some(format!(
        "the API's rate limit {} (anonymous reads share 60 an hour per public address){}",
        if exhausted { "is spent" } else { "refused it" },
        wait.map_or_else(String::new, |s| format!("; it resets in {s} s"))
    ))
}

/// The absence of `asset` from its listed release.
#[cfg_attr(trust_verify, trust::skip)]
fn not_listed(asset: &Asset<'_>) -> String {
    format!("{asset}: the API's release lists no asset of that name")
}

/// `<dest>.part`.
fn part_path(dest: &Path) -> Option<PathBuf> {
    let mut name = dest.file_name()?.to_os_string();
    name.push(".part");
    Some(dest.with_file_name(name))
}

/// The sink's length so far; 0 when there is none.
fn sink_len(sink: &Path) -> u64 {
    std::fs::symlink_metadata(sink).map_or(0, |m| m.len())
}

/// The delivered body, at most `cap` bytes.
#[cfg_attr(trust_verify, trust::skip)]
fn read_capped(body: &Path, cap: u64) -> Result<Vec<u8>, AssetError> {
    use std::io::Read as _;
    let (file, _) = aterm_uds::regular_file::open(body)
        .map_err(|e| AssetError::Refused(format!("read the delivered asset: {e}")))?;
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| AssetError::Refused(format!("read the delivered asset: {e}")))?;
    if bytes.len() as u64 > cap {
        return Err(AssetError::Refused(format!(
            "the delivered asset exceeds its {cap}-byte cap"
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "release_asset_tests.rs"]
mod tests;
