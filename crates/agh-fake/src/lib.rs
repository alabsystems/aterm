// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Loopback HTTP fake for GitHub API, uploads, web, and CDN origins used by `agh` tests: its
//! reads, its release, asset, ref and key writes (with the faults that land a write and lose
//! its answer, the listing lag and immutable releases), and the web origin's smart-HTTP git
//! endpoints (protocol v0, delete-only pushes).

#![forbid(unsafe_code)]
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![warn(missing_docs)]

mod git_http;
mod routes;
mod server;
mod state;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use state::{Rule, State};

pub(crate) fn parse_uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn seeded_digest(domain: &[u8], seed: &str) -> [u8; 32] {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(domain);
    hasher.update(seed.as_bytes());
    hasher.finalize()
}

pub(crate) fn sha256_prefixed(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    aterm_codec::hex::push_lower(&mut out, &aterm_digest::Sha256::digest(bytes));
    out
}

/// Generates a deterministic classic test token (`ghp_...`) from `seed`.
#[must_use]
pub fn token(seed: &str) -> String {
    let digest = seeded_digest(b"agh-fake\0", seed);
    let mut out = String::with_capacity(4 + 36);
    out.push_str("ghp_");
    aterm_codec::hex::push_lower(&mut out, &digest[..18]);
    out
}

/// Generates a deterministic fine-grained test token (`github_pat_...`) from `seed`.
#[must_use]
pub fn fine_grained_token(seed: &str) -> String {
    let digest = seeded_digest(b"agh-fake/fine-grained\0", seed);
    let mut out = String::with_capacity(11 + 64);
    out.push_str("github_pat_");
    aterm_codec::hex::push_lower(&mut out, &digest);
    out
}

/// An asset as [`FakeGithub::release_state`] reports it: its name and its bytes.
pub type NamedBytes = (String, Vec<u8>);

/// Loopback ports bound by the four fake origins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ports {
    /// API origin port.
    pub api: u16,
    /// Uploads origin port.
    pub uploads: u16,
    /// Web origin port.
    pub web: u16,
    /// CDN origin port.
    pub cdn: u16,
}

/// Origin reached by a recorded request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeOrigin {
    /// API origin.
    Api,
    /// Uploads origin.
    Uploads,
    /// Web origin.
    Web,
    /// CDN origin.
    Cdn,
}

/// Running loopback GitHub fake whose listeners stop on drop.
pub struct FakeGithub {
    state: Arc<Mutex<State>>,
    ports: Ports,
    stop: Arc<AtomicBool>,
    accepts: Vec<JoinHandle<()>>,
}

/// Repository specification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSpec {
    /// Whether the repository is private.
    pub private: bool,
    /// Default branch name.
    pub default_branch: String,
    /// Logins granted push access.
    pub push_logins: Vec<String>,
    /// Logins granted pull access.
    pub pull_logins: Vec<String>,
}

impl RepoSpec {
    /// Public repository with default branch `main`.
    #[must_use]
    pub fn public() -> RepoSpec {
        RepoSpec {
            private: false,
            default_branch: "main".to_string(),
            push_logins: Vec::new(),
            pull_logins: Vec::new(),
        }
    }

    /// Private repository with default branch `main`.
    #[must_use]
    pub fn private() -> RepoSpec {
        RepoSpec {
            private: true,
            ..RepoSpec::public()
        }
    }

    /// Grants push access to `login`.
    #[must_use]
    pub fn push(mut self, login: &str) -> RepoSpec {
        self.push_logins.push(login.to_string());
        self
    }

    /// Grants pull access to `login`.
    #[must_use]
    pub fn pull(mut self, login: &str) -> RepoSpec {
        self.pull_logins.push(login.to_string());
        self
    }
}

/// Release specification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseSpec {
    /// Release tag.
    pub tag: String,
    /// Release title.
    pub name: Option<String>,
    /// Whether the release is a draft.
    pub draft: bool,
    /// Whether the release is a prerelease.
    pub prerelease: bool,
    /// Whether the release is immutable.
    pub immutable: bool,
    /// Target commitish.
    pub target_commitish: String,
    /// Creation timestamp.
    pub created_at: String,
    /// Publication timestamp.
    pub published_at: Option<String>,
    /// Release notes body.
    pub body: Option<String>,
}

impl ReleaseSpec {
    /// Published release at `created_at` targeting `main`.
    #[must_use]
    pub fn published(tag: &str, created_at: &str) -> ReleaseSpec {
        ReleaseSpec {
            tag: tag.to_string(),
            name: None,
            draft: false,
            prerelease: false,
            immutable: false,
            target_commitish: "main".to_string(),
            created_at: created_at.to_string(),
            published_at: Some(created_at.to_string()),
            body: None,
        }
    }

    /// Draft release with no `published_at`.
    #[must_use]
    pub fn draft(tag: &str, created_at: &str) -> ReleaseSpec {
        ReleaseSpec {
            draft: true,
            published_at: None,
            ..ReleaseSpec::published(tag, created_at)
        }
    }

    /// Sets the `prerelease` flag.
    #[must_use]
    pub const fn prerelease(mut self, prerelease: bool) -> ReleaseSpec {
        self.prerelease = prerelease;
        self
    }
}

/// Asset `digest` field behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigestSpec {
    /// True SHA-256 digest of the asset bytes.
    True,
    /// `null` digest.
    Null,
    /// Mismatched SHA-256 digest.
    Wrong,
}

/// Release asset specification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetSpec {
    /// Asset file name.
    pub name: String,
    /// Asset payload bytes.
    pub bytes: Vec<u8>,
    /// Asset state (`uploaded`, `open`, `starter`, etc.).
    pub state: String,
    /// Digest behavior.
    pub digest: DigestSpec,
    /// Content media type.
    pub content_type: String,
    /// Last-updated timestamp.
    pub updated_at: String,
}

impl AssetSpec {
    /// Uploaded `application/octet-stream` asset with its true digest.
    #[must_use]
    pub fn uploaded(name: &str, bytes: &[u8]) -> AssetSpec {
        AssetSpec {
            name: name.to_string(),
            bytes: bytes.to_vec(),
            state: "uploaded".to_string(),
            digest: DigestSpec::True,
            content_type: "application/octet-stream".to_string(),
            updated_at: "2026-10-01T00:00:00Z".to_string(),
        }
    }
}

/// Commit specification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitSpec {
    /// Commit SHA.
    pub sha: String,
    /// Commit message.
    pub message: String,
    /// Author GitHub login.
    pub author_login: Option<String>,
    /// Author Git name.
    pub author_name: String,
    /// Committer timestamp.
    pub date: String,
    /// Paths modified by the commit.
    pub paths: Vec<String>,
}

/// Canned response for `compare/{base}...{head}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompareSpec {
    /// Successful comparison result.
    Ok {
        /// Comparison status (`ahead`, `behind`, `diverged`, or `identical`).
        status: String,
        /// Commits `head` is ahead of `base`.
        ahead_by: u64,
        /// Commits `head` is behind `base`.
        behind_by: u64,
        /// Total commits in the comparison.
        total_commits: u64,
        /// Merge-base commit SHA.
        merge_base: String,
    },
    /// 404 `No common ancestor` response.
    NoCommonAncestor,
}

/// Request matcher for faults and state changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Target origin.
    pub origin: FakeOrigin,
    /// Optional HTTP method filter.
    pub method: Option<String>,
    /// Request path prefix.
    pub path_prefix: String,
}

impl Match {
    /// Matches API requests under `path_prefix`.
    #[must_use]
    pub fn api(path_prefix: &str) -> Match {
        Match {
            origin: FakeOrigin::Api,
            method: None,
            path_prefix: path_prefix.to_string(),
        }
    }

    /// Matches uploads requests under `path_prefix`.
    #[must_use]
    pub fn uploads(path_prefix: &str) -> Match {
        Match {
            origin: FakeOrigin::Uploads,
            ..Match::api(path_prefix)
        }
    }

    /// Matches web requests under `path_prefix`.
    #[must_use]
    pub fn web(path_prefix: &str) -> Match {
        Match {
            origin: FakeOrigin::Web,
            ..Match::api(path_prefix)
        }
    }

    /// Matches all CDN requests.
    #[must_use]
    pub fn cdn() -> Match {
        Match {
            origin: FakeOrigin::Cdn,
            ..Match::api("/")
        }
    }

    /// Restricts this matcher to HTTP `method`.
    #[must_use]
    pub fn method(mut self, method: &str) -> Match {
        self.method = Some(method.to_string());
        self
    }
}

/// Injected fault action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Responds with custom status, body, and headers.
    Status {
        /// HTTP status code.
        status: u16,
        /// Response body.
        body: String,
        /// Extra response headers.
        headers: Vec<(String, String)>,
    },
    /// Closes the connection after reading the request without sending a response.
    Close,
    /// Holds the connection open for up to the given duration before closing.
    Hang(std::time::Duration),
    /// Sends the full response header and truncates the body at `at` bytes.
    TruncateBody {
        /// Number of body bytes sent.
        at: usize,
    },
    /// Responds with 403 secondary rate limit and `retry-after`.
    SecondaryLimit {
        /// `retry-after` header value in seconds.
        retry_after: u64,
    },
    /// Responds with 403 primary rate limit exhausted (`x-ratelimit-remaining: 0`).
    PrimaryExhausted {
        /// `x-ratelimit-reset` Unix timestamp.
        reset: u64,
    },
    /// Responds with a 3xx redirect to `location`.
    RedirectTo {
        /// Redirect HTTP status code.
        status: u16,
        /// `Location` header value.
        location: String,
    },
    /// Answers normally and then deletes the repository's newest release.
    Torn,
    /// Redirects a CDN request to the CDN a second time (302).
    SecondHop,
    /// Apply the write, then close with no answer: the model's `LandedLost`.
    LandThenClose,
    /// Apply the write, then answer `status` with GitHub's error body for it: a 502 whose
    /// write landed.
    LandThenStatus {
        /// The status.
        status: u16,
    },
    /// An upload: create the asset in state `starter`, then answer `status`.
    StarterThen {
        /// The status.
        status: u16,
    },
}

/// State mutation applied by the fake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Marks a draft release as published.
    Publish {
        /// Target release ID.
        release_id: u64,
    },
    /// Adds or replaces an asset on a release.
    AddAsset {
        /// Target release ID.
        release_id: u64,
        /// Asset specification.
        spec: AssetSpec,
    },
    /// Overrides the repository's `releases/latest` target.
    SetLatest {
        /// Repository `owner/name` slug.
        slug: String,
        /// Target release ID, or `None` for 404.
        release_id: Option<u64>,
    },
    /// Deletes a release.
    DeleteRelease {
        /// Target release ID.
        release_id: u64,
    },
    /// Makes the repository private with no authorized logins.
    Hide {
        /// Repository `owner/name` slug.
        slug: String,
    },
    /// A release the listing lag hid becomes visible.
    Reveal {
        /// The release.
        release_id: u64,
    },
}

/// The Basic credential git sends for `token` under agh's helper username: the standard
/// base64, padded, of `x-access-token:{token}`. Tests build it at runtime (§1, rule 7).
#[must_use]
pub fn basic(token: &str) -> String {
    aterm_codec::base64::encode(format!("x-access-token:{token}").as_bytes()).unwrap_or_default()
}

/// A write's body as the fake received it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenBody {
    /// Its request's index in `seen()`.
    pub seen: usize,
    /// Its length.
    pub len: u64,
    /// `sha256:{hex}` of its bytes.
    pub sha256: String,
    /// Its text, when it is JSON of at most 64 KiB.
    pub json: Option<String>,
}

/// One SSH key the fake holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeKey {
    /// The id, from 20000.
    pub id: u64,
    /// The title.
    pub title: String,
    /// `<type> <base64>`.
    pub key: String,
}

/// A release as the fake holds it, hidden ones included: what a test asserts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeRelease {
    /// The id.
    pub id: u64,
    /// The tag.
    pub tag: String,
    /// Whether it is a draft.
    pub draft: bool,
    /// Whether it is a prerelease.
    pub prerelease: bool,
    /// Whether it is immutable.
    pub immutable: bool,
    /// Whether the listing lag hides it.
    pub hidden: bool,
    /// The `target_commitish` the create sent, if any.
    pub target_commitish: Option<String>,
    /// The notes.
    pub body: String,
    /// The `make_latest` the last create or PATCH sent, if any.
    pub make_latest: Option<String>,
    /// Its assets: id, name, state and `sha256:{hex}`.
    pub assets: Vec<(u64, String, String, String)>,
}

/// Recorded incoming request with credentials redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// Origin that received the request.
    pub origin: FakeOrigin,
    /// HTTP method.
    pub method: String,
    /// Request path and query with registered tokens redacted to `<token>`.
    pub path_and_query: String,
    /// Request headers excluding credential headers.
    pub headers: Vec<(String, String)>,
    /// Whether the request carried an authorization credential.
    pub had_authorization: bool,
    /// Login associated with the request token, if known.
    pub as_login: Option<String>,
}

impl Seen {
    /// Looks up header `name` ASCII case-insensitively.
    #[inline]
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header_in(&self.headers, name)
    }
}

/// Looks up `name` in `headers` ASCII case-insensitively.
#[inline]
pub(crate) fn header_in<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

impl FakeGithub {
    /// Binds four loopback listeners and serves each on a background thread.
    /// # Panics
    /// Panics if binding a loopback listener or spawning its thread fails.
    #[must_use]
    pub fn start() -> FakeGithub {
        let state = Arc::new(Mutex::new(State::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (ports, accepts) = server::serve(&state, &stop);
        FakeGithub {
            state,
            ports,
            stop,
            accepts,
        }
    }

    /// Returns the bound loopback [`Ports`].
    #[inline]
    #[must_use]
    pub const fn ports(&self) -> Ports {
        self.ports
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers `token` for user `login` on the API and uploads origins.
    pub fn add_token(&self, token: &str, login: &str) {
        self.state().add_token(token, login);
    }

    /// Adds repository `slug` (`owner/name`) and returns its numeric ID.
    /// # Panics
    /// Panics if `slug` is not `owner/name`.
    pub fn add_repo(&self, slug: &str, spec: RepoSpec) -> u64 {
        self.state().add_repo(slug, spec)
    }

    /// Renames repository `old` to `new`, redirecting old paths with 301.
    /// # Panics
    /// Panics if `old` does not exist or `new` is not `owner/name`.
    pub fn rename_repo(&self, old: &str, new: &str) {
        self.state().rename_repo(old, new);
    }

    /// Adds a release to repository `slug` and returns its numeric ID.
    /// # Panics
    /// Panics if repository `slug` does not exist.
    pub fn add_release(&self, slug: &str, spec: ReleaseSpec) -> u64 {
        self.state().add_release(slug, spec)
    }

    /// Adds or replaces an asset on `release_id` and returns its numeric ID.
    /// # Panics
    /// Panics if `release_id` does not exist.
    pub fn add_asset(&self, release_id: u64, spec: AssetSpec) -> u64 {
        self.state().add_asset(release_id, spec)
    }

    /// Overrides which release `releases/latest` serves for `slug`.
    /// # Panics
    /// Panics if repository `slug` does not exist.
    pub fn set_latest(&self, slug: &str, release_id: Option<u64>) {
        self.state().set_latest(slug, release_id);
    }

    /// Stores file `bytes` at `path` under `reference` in repository `slug`.
    /// # Panics
    /// Panics if repository `slug` does not exist.
    pub fn put_file(&self, slug: &str, reference: &str, path: &str, bytes: &[u8]) {
        self.state().put_file(slug, reference, path, bytes);
    }

    /// Sets whether git tree responses for `slug` report `truncated: true`.
    /// # Panics
    /// Panics if repository `slug` does not exist.
    pub fn set_tree_truncated(&self, slug: &str, truncated: bool) {
        self.state().set_tree_truncated(slug, truncated);
    }

    /// Appends a commit to repository `slug`.
    /// # Panics
    /// Panics if repository `slug` does not exist.
    pub fn add_commit(&self, slug: &str, spec: CommitSpec) {
        self.state().add_commit(slug, spec);
    }

    /// Points branch `name` in `slug` at `sha`.
    /// # Panics
    /// Panics if repository `slug` does not exist.
    pub fn set_branch(&self, slug: &str, name: &str, sha: &str) {
        self.state().set_branch(slug, name, sha);
    }

    /// Points tag `name` in `slug` at `sha`.
    /// # Panics
    /// Panics if repository `slug` does not exist.
    pub fn set_tag(&self, slug: &str, name: &str, sha: &str) {
        self.state().set_tag(slug, name, sha);
    }

    /// Sets the canned response for `compare/{base}...{head}` in `slug`.
    /// # Panics
    /// Panics if repository `slug` does not exist.
    pub fn set_compare(&self, slug: &str, base: &str, head: &str, spec: CompareSpec) {
        self.state().set_compare(slug, base, head, spec);
    }

    /// Applies `change` immediately.
    /// # Panics
    /// Panics if `change` references a missing repository or release.
    pub fn apply(&self, change: Change) {
        self.state().apply(change);
    }

    /// Schedules `change` to run after the `count`-th request matching `m`.
    /// # Panics
    /// Panics if `change` names a release or repository the fake does not hold now.
    pub fn after(&self, m: Match, count: usize, change: Change) {
        self.state().after(m, count, change);
    }

    /// Queues a one-shot `fault` for the next request matching `m`.
    pub fn fault(&self, m: Match, fault: Fault) {
        self.state().rules.push(Rule {
            m,
            fault,
            always: false,
        });
    }

    /// Ends every [`Fault::Hang`], held now or asked for later.
    pub fn end_hangs(&self) {
        self.state().hangs_ended = true;
    }

    /// Installs a standing `fault` for all requests matching `m`.
    pub fn fault_always(&self, m: Match, fault: Fault) {
        self.state().rules.push(Rule {
            m,
            fault,
            always: true,
        });
    }

    /// The ids of `slug`'s releases, in the order added (a write's among them).
    #[must_use]
    pub fn release_ids(&self, slug: &str) -> Vec<u64> {
        let state = self.state();
        let Some(repo) = state.repo_index(slug).map(|i| state.repos[i].id) else {
            return Vec::new();
        };
        state
            .releases
            .iter()
            .filter(|r| r.repo == repo)
            .map(|r| r.id)
            .collect()
    }

    /// Release `release_id` as the fake holds it now.
    #[must_use]
    pub fn release_state(&self, release_id: u64) -> Option<(ReleaseSpec, Vec<NamedBytes>)> {
        let state = self.state();
        let release = state.releases.iter().find(|r| r.id == release_id)?;
        let assets = release
            .assets
            .iter()
            .map(|a| (a.spec.name.clone(), a.spec.bytes.clone()))
            .collect();
        Some((release.spec.clone(), assets))
    }

    /// Returns all recorded requests in arrival order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        self.state().seen.clone()
    }

    /// Returns all recorded protocol violations and stale `after` changes.
    #[must_use]
    pub fn violations(&self) -> Vec<String> {
        self.state().violations.clone()
    }

    /// Asserts that no wire-rule violations have occurred.
    /// # Panics
    /// Panics if any violation was recorded.
    pub fn assert_clean(&self) {
        let violations = self.violations();
        assert!(violations.is_empty(), "{violations:?}");
    }

    /// Clears the recorded request log and the write bodies captured with it, so each
    /// `SeenBody::seen` keeps indexing `seen()`.
    pub fn clear_seen(&self) {
        let mut state = self.state();
        state.seen.clear();
        state.bodies.clear();
    }

    /// `X-OAuth-Scopes` for `token`'s answers. A classic token without `admin:public_key`
    /// gets 404 on every `/user/keys` call, as GitHub answers.
    pub fn set_scopes(&self, token: &str, scopes: &[&str]) {
        let mut state = self.state();
        state.scopes.retain(|(t, _)| t != token);
        state.scopes.push((
            token.to_string(),
            scopes.iter().map(|s| (*s).to_string()).collect(),
        ));
    }

    /// Stand `token`'s core rate at `used` requests, as that many charged calls leave it: a
    /// test reaches the limit without making them, each of which leaves a TIME_WAIT socket
    /// for 30 s (macOS) once the fake closes it.
    /// # Panics
    /// Panics if `used` passes a token's core limit.
    pub fn set_rate_used(&self, token: &str, used: u64) {
        let limit = routes::limit_for(true, "core");
        assert!(
            used <= limit,
            "agh-fake: {used} requests pass a token's limit of {limit}"
        );
        self.state()
            .used
            .insert((Some(token.to_string()), "core"), used);
    }

    /// Make release `release_id` immutable, or not.
    /// # Panics
    /// Panics if `release_id` does not exist.
    pub fn set_immutable(&self, release_id: u64, immutable: bool) {
        let mut state = self.state();
        let index = state
            .release_index(release_id)
            .unwrap_or_else(|| panic!("agh-fake: no release {release_id}"));
        state.releases[index].spec.immutable = immutable;
    }

    /// While on, a release POSTed to `slug`, or added with `add_release`, is hidden from every
    /// listing and by-tag read (the API's `releases/tags/{tag}`, `releases/latest` and the web
    /// origin's download by tag) until `Change::Reveal` names it. A by-id read still sees it,
    /// as GitHub's does.
    pub fn set_listing_lag(&self, slug: &str, on: bool) {
        let mut state = self.state();
        state.listing_lag.retain(|s| !s.eq_ignore_ascii_case(slug));
        if on {
            state.listing_lag.push(slug.to_string());
        }
    }

    /// Give `login` an SSH key, and answer its id. The key is kept as GitHub keeps one, its
    /// type and base64 without the comment, as a POSTed key is.
    pub fn add_key(&self, login: &str, title: &str, key: &str) -> u64 {
        self.state().add_key(login, title, key)
    }

    /// `login`'s keys, by id.
    #[must_use]
    pub fn keys(&self, login: &str) -> Vec<FakeKey> {
        let state = self.state();
        let mut keys: Vec<FakeKey> = state
            .keys
            .iter()
            .filter(|k| k.login.eq_ignore_ascii_case(login))
            .map(|k| k.key.clone())
            .collect();
        keys.sort_unstable_by_key(|k| k.id);
        keys
    }

    /// Every write body so far, in order.
    #[must_use]
    pub fn bodies(&self) -> Vec<SeenBody> {
        self.state().bodies.clone()
    }

    /// Every release of `slug`, by id, hidden ones included.
    #[must_use]
    pub fn releases_of(&self, slug: &str) -> Vec<FakeRelease> {
        let state = self.state();
        let Some(repo) = state.repo_index(slug).map(|i| state.repos[i].id) else {
            return Vec::new();
        };
        let mut releases: Vec<FakeRelease> = state
            .releases
            .iter()
            .filter(|r| r.repo == repo)
            .map(|r| FakeRelease {
                id: r.id,
                tag: r.spec.tag.clone(),
                draft: r.spec.draft,
                prerelease: r.spec.prerelease,
                immutable: r.spec.immutable,
                hidden: r.hidden,
                target_commitish: r.sent_target.clone(),
                body: r.spec.body.clone().unwrap_or_default(),
                make_latest: r.make_latest.clone(),
                assets: r
                    .assets
                    .iter()
                    .map(|a| {
                        (
                            a.id,
                            a.spec.name.clone(),
                            a.spec.state.clone(),
                            sha256_prefixed(&a.spec.bytes),
                        )
                    })
                    .collect(),
            })
            .collect();
        releases.sort_unstable_by_key(|r| r.id);
        releases
    }

    /// `slug`'s fully qualified refs with their OIDs, sorted.
    #[must_use]
    pub fn git_refs(&self, slug: &str) -> Vec<(String, String)> {
        let state = self.state();
        state
            .repo_index(slug)
            .map_or_else(Vec::new, |index| state.repos[index].refs())
    }
}

impl Drop for FakeGithub {
    fn drop(&mut self) {
        self.end_hangs();
        self.stop.store(true, Ordering::SeqCst);
        for accept in &self.accepts {
            accept.thread().unpark();
        }
        server::wake(self.ports);
        for accept in self.accepts.drain(..) {
            let _ = accept.join();
        }
    }
}
