// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Loopback HTTP fake for GitHub API, uploads, web, and CDN origins used by `agh` tests.

#![forbid(unsafe_code)]
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![warn(missing_docs)]

mod routes;
mod server;
mod state;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use state::{Rule, State};

/// sha256 of `domain` ‖ `seed`, as lowercase hex.
fn seeded_hex(domain: &[u8], seed: &str) -> String {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(domain);
    hasher.update(seed.as_bytes());
    aterm_codec::hex::encode_lower(&hasher.finalize())
}

/// Generates a deterministic classic test token (`ghp_...`) from `seed`.
#[must_use]
pub fn token(seed: &str) -> String {
    let mut hex = seeded_hex(b"agh-fake\0", seed);
    hex.truncate(36);
    format!("ghp_{hex}")
}

/// Generates a deterministic fine-grained test token (`github_pat_...`) from `seed`.
#[must_use]
pub fn fine_grained_token(seed: &str) -> String {
    format!(
        "github_pat_{}",
        seeded_hex(b"agh-fake/fine-grained\0", seed)
    )
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

    /// The state, under its lock.
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

    /// Ends every [`Fault::Hang`], held now or asked for later: each connection closes as
    /// though its time had run out.
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

    /// Release `release_id` as the fake holds it now: what it says, and each asset's name and
    /// bytes in the order added; `None` once it is deleted.
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

    /// Clears the recorded request log.
    pub fn clear_seen(&self) {
        self.state().seen.clear();
    }
}

impl Drop for FakeGithub {
    fn drop(&mut self) {
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
