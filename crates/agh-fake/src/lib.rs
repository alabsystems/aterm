// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A loopback GitHub for agh's tests: the api, uploads, web and CDN origins, each a plain
//! HTTP listener on 127.0.0.1, with the faults a test asks for and a log of what it was sent.
//!
//! - **Wire.** HTTP/1.1, one request per connection (`Connection: close`). Every answer
//!   carries `Content-Length` but a 304; a HEAD carries its GET's. Every api answer carries
//!   `X-GitHub-Request-Id: FAKE:{n}` (n counts requests from 1) and the `x-ratelimit-*`
//!   headers (5000 per token, 60 anonymous; an authenticated 304 and `/rate_limit` are free).
//!   An api request the rate charges, made once its limit is used up, answers 403
//!   `API rate limit exceeded.`, as GitHub's does.
//! - **ETags.** A JSON GET's 200 carries `ETag: W/"{16 hex of sha256(body)}"`; a matching
//!   `If-None-Match` answers 304.
//! - **Visibility.** A private repository is a 404 to anyone it does not name; drafts are
//!   listed only to a push login.
//! - **Ids.** Owners count from 100, repositories from 1000, releases from 5000, assets
//!   from 9000.
//! - **Violations.** A credential at the web or CDN origin (the CDN then answers 400), a
//!   missing or empty `User-Agent`, a missing or empty `X-GitHub-Api-Version` at the api or
//!   uploads origin, an `X-GitHub-*` header at the web or CDN origin, `Content-Length` on a
//!   GET or HEAD, and `per_page` over 100, each recorded as one line of `violations()`. At
//!   the web and CDN origins a credential is `Authorization`, `Proxy-Authorization`,
//!   `Cookie`, or any header whose value holds a token `add_token` registered, one line
//!   each. A registered token in the request target is one at every origin (the CDN answers
//!   it 400 too), and more than one credential header at the api or uploads origin is one,
//!   answered 400. Every text, and `seen()`'s target, shows a registered token as `<token>`.
//!   A request head the fake cannot parse (no target, a header line without `:`, a head cut
//!   short or over 64 KiB, the request line included) is one too, logged in `seen()` and
//!   answered 400. An `after` change that has gone stale by the time it is due (its release
//!   deleted since) is skipped and recorded as one.

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

/// A token for tests: `ghp_` and the first 36 lowercase hex digits of sha256(`agh-fake\0` ‖
/// `seed`). No test spells a token-shaped literal.
#[must_use]
pub fn token(seed: &str) -> String {
    let mut hex = seeded_hex(b"agh-fake\0", seed);
    hex.truncate(36);
    format!("ghp_{hex}")
}

/// A fine-grained token for tests: `github_pat_` and the 64 lowercase hex digits of
/// sha256(`agh-fake/fine-grained\0` ‖ `seed`).
#[must_use]
pub fn fine_grained_token(seed: &str) -> String {
    format!(
        "github_pat_{}",
        seeded_hex(b"agh-fake/fine-grained\0", seed)
    )
}

/// The four ports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ports {
    /// The api origin.
    pub api: u16,
    /// The uploads origin.
    pub uploads: u16,
    /// The web origin.
    pub web: u16,
    /// The CDN origin.
    pub cdn: u16,
}

/// Which origin a request reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeOrigin {
    /// The api origin.
    Api,
    /// The uploads origin.
    Uploads,
    /// The web origin.
    Web,
    /// The CDN origin.
    Cdn,
}

/// The running fake. Dropping it stops the listeners.
pub struct FakeGithub {
    state: Arc<Mutex<State>>,
    ports: Ports,
    stop: Arc<AtomicBool>,
    accepts: Vec<JoinHandle<()>>,
}

/// A repository to add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSpec {
    /// Whether it is private.
    pub private: bool,
    /// Its default branch.
    pub default_branch: String,
    /// The logins with push.
    pub push_logins: Vec<String>,
    /// The logins with pull.
    pub pull_logins: Vec<String>,
}

impl RepoSpec {
    /// Public, default branch `main`, nobody with push.
    #[must_use]
    pub fn public() -> RepoSpec {
        RepoSpec {
            private: false,
            default_branch: "main".to_string(),
            push_logins: Vec::new(),
            pull_logins: Vec::new(),
        }
    }

    /// As `public`, but private.
    #[must_use]
    pub fn private() -> RepoSpec {
        RepoSpec {
            private: true,
            ..RepoSpec::public()
        }
    }

    /// The same repository with `login` holding push.
    #[must_use]
    pub fn push(mut self, login: &str) -> RepoSpec {
        self.push_logins.push(login.to_string());
        self
    }

    /// The same repository with `login` holding pull.
    #[must_use]
    pub fn pull(mut self, login: &str) -> RepoSpec {
        self.pull_logins.push(login.to_string());
        self
    }
}

/// A release to add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseSpec {
    /// The tag.
    pub tag: String,
    /// The title.
    pub name: Option<String>,
    /// Whether it is a draft.
    pub draft: bool,
    /// Whether it is a prerelease.
    pub prerelease: bool,
    /// Whether it is immutable.
    pub immutable: bool,
    /// The commitish.
    pub target_commitish: String,
    /// When it was made.
    pub created_at: String,
    /// When it was published.
    pub published_at: Option<String>,
    /// The notes.
    pub body: Option<String>,
}

impl ReleaseSpec {
    /// Published at `created_at`, target `main`, no name or body.
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

    /// A draft (no `published_at`).
    #[must_use]
    pub fn draft(tag: &str, created_at: &str) -> ReleaseSpec {
        ReleaseSpec {
            draft: true,
            published_at: None,
            ..ReleaseSpec::published(tag, created_at)
        }
    }

    /// The same release, a prerelease or not.
    #[must_use]
    pub fn prerelease(mut self, prerelease: bool) -> ReleaseSpec {
        self.prerelease = prerelease;
        self
    }
}

/// What an asset's `digest` field says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigestSpec {
    /// The bytes' true digest.
    True,
    /// `null`, as assets before June 2025 have.
    Null,
    /// A digest of other bytes.
    Wrong,
}

/// An asset to add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetSpec {
    /// The file name.
    pub name: String,
    /// The bytes.
    pub bytes: Vec<u8>,
    /// `uploaded`, `open`, `starter`, ….
    pub state: String,
    /// What `digest` says.
    pub digest: DigestSpec,
    /// The media type.
    pub content_type: String,
    /// When it last changed.
    pub updated_at: String,
}

impl AssetSpec {
    /// `uploaded`, the true digest, `application/octet-stream`, updated
    /// `2026-10-01T00:00:00Z`.
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

/// A commit to add, newest last; history is linear in the order added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitSpec {
    /// The sha.
    pub sha: String,
    /// The message.
    pub message: String,
    /// The author's GitHub login.
    pub author_login: Option<String>,
    /// The author's git name.
    pub author_name: String,
    /// The committer date.
    pub date: String,
    /// The paths it touches.
    pub paths: Vec<String>,
}

/// What `compare/{base}...{head}` answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompareSpec {
    /// A comparison.
    Ok {
        /// `ahead`, `behind`, `diverged` or `identical`.
        status: String,
        /// Commits head has that base lacks.
        ahead_by: u64,
        /// Commits base has that head lacks.
        behind_by: u64,
        /// Every commit of the comparison.
        total_commits: u64,
        /// The merge base's sha.
        merge_base: String,
    },
    /// 404 `No common ancestor between {base} and {head}.`
    NoCommonAncestor,
}

/// Which requests a fault or a change applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// The origin.
    pub origin: FakeOrigin,
    /// The method, or any.
    pub method: Option<String>,
    /// The path's prefix.
    pub path_prefix: String,
}

impl Match {
    /// Api requests under `path_prefix`.
    #[must_use]
    pub fn api(path_prefix: &str) -> Match {
        Match {
            origin: FakeOrigin::Api,
            method: None,
            path_prefix: path_prefix.to_string(),
        }
    }

    /// Uploads requests under `path_prefix`.
    #[must_use]
    pub fn uploads(path_prefix: &str) -> Match {
        Match {
            origin: FakeOrigin::Uploads,
            ..Match::api(path_prefix)
        }
    }

    /// Web requests under `path_prefix`.
    #[must_use]
    pub fn web(path_prefix: &str) -> Match {
        Match {
            origin: FakeOrigin::Web,
            ..Match::api(path_prefix)
        }
    }

    /// Every CDN request.
    #[must_use]
    pub fn cdn() -> Match {
        Match {
            origin: FakeOrigin::Cdn,
            ..Match::api("/")
        }
    }

    /// The same match, for `method` only.
    #[must_use]
    pub fn method(mut self, method: &str) -> Match {
        self.method = Some(method.to_string());
        self
    }
}

/// A fault: what the fake does instead of answering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// This status, body and headers (plus the rate headers on the api origin). The body is
    /// served as JSON unless `headers` names a `Content-Type`. A header named here replaces
    /// the fake's own of that name (`x-ratelimit-*`, `X-GitHub-Request-Id`), except
    /// `Content-Length` and `Connection`, which the fake always writes itself and drops here.
    Status {
        /// The status.
        status: u16,
        /// The body.
        body: String,
        /// The headers.
        headers: Vec<(String, String)>,
    },
    /// Read the request, then close without a byte of answer.
    Close,
    /// Read the request, wait this long, then close. The fake waits on the client, not on a
    /// clock, so the hang also ends as soon as the client hangs up.
    Hang(std::time::Duration),
    /// The full head with the true `Content-Length`, then only `at` bytes of the body.
    TruncateBody {
        /// The bytes sent.
        at: usize,
    },
    /// 403, `retry-after`, and a message holding `secondary rate limit`.
    SecondaryLimit {
        /// The `retry-after` seconds.
        retry_after: u64,
    },
    /// 403 with `x-ratelimit-remaining: 0` and `x-ratelimit-reset`, on whichever origin it
    /// fires (the five `x-ratelimit-*` headers, the limit used up).
    PrimaryExhausted {
        /// The reset, in Unix seconds.
        reset: u64,
    },
    /// This 3xx status and `Location`.
    RedirectTo {
        /// The status.
        status: u16,
        /// The `Location`.
        location: String,
    },
    /// Answer normally, then delete the repository's newest release.
    Torn,
    /// On the CDN: answer 302 to the CDN again.
    SecondHop,
}

/// A change to the fake's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A draft becomes published (`published_at = created_at`).
    Publish {
        /// The release.
        release_id: u64,
    },
    /// As `add_asset`. An asset of that name already on the release is replaced in place: its
    /// id is kept, and its bytes, size, digest, state and `updated_at` come from `spec`.
    AddAsset {
        /// The release.
        release_id: u64,
        /// The asset.
        spec: AssetSpec,
    },
    /// As `set_latest`.
    SetLatest {
        /// The repository.
        slug: String,
        /// The release, or none.
        release_id: Option<u64>,
    },
    /// The release is deleted.
    DeleteRelease {
        /// The release.
        release_id: u64,
    },
    /// The repository becomes private with no logins: a 404 to everyone.
    Hide {
        /// The repository.
        slug: String,
    },
}

/// One request as the fake received it. `headers` holds every header but the credential
/// header (at the web and CDN origins, every header `violations()` counts as a credential),
/// which is reduced to `had_authorization` and `as_login`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// The origin it reached.
    pub origin: FakeOrigin,
    /// The method.
    pub method: String,
    /// The path and query, with every token `add_token` registered in it shown as `<token>`.
    pub path_and_query: String,
    /// Every header but the credential (and, at the web and CDN origins, every header the
    /// violations count as one: `Proxy-Authorization`, `Cookie`, a registered token's).
    pub headers: Vec<(String, String)>,
    /// Whether it carried a credential header (at the web and CDN origins, any credential,
    /// a registered token in the target among them).
    pub had_authorization: bool,
    /// The login its token is, when the fake knows it.
    pub as_login: Option<String>,
}

impl Seen {
    /// The value of header `name`, ASCII case-insensitively.
    #[inline]
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header_in(&self.headers, name)
    }
}

/// The value of header `name` among `headers`, ASCII case-insensitively: the one lookup the
/// answers, the requests, the server and the seen log share.
#[inline]
pub(crate) fn header_in<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

impl FakeGithub {
    /// Bind 127.0.0.1:0 four times and serve each listener on its own thread.
    ///
    /// # Panics
    ///
    /// When a loopback port cannot be bound: a test cannot run without one.
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

    /// The four ports.
    #[inline]
    #[must_use]
    pub const fn ports(&self) -> Ports {
        self.ports
    }

    /// The state, under its lock.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A token the api and uploads origins accept, as `login` (an owner of type User).
    pub fn add_token(&self, token: &str, login: &str) {
        self.state().add_token(token, login);
    }

    /// `owner/name`; the id. An owner that is some token's login is a User, any other an
    /// Organization.
    ///
    /// # Panics
    ///
    /// When `slug` holds no `/`.
    pub fn add_repo(&self, slug: &str, spec: RepoSpec) -> u64 {
        self.state().add_repo(slug, spec)
    }

    /// From now on the old name answers 301 to `/repositories/{id}…`.
    ///
    /// # Panics
    ///
    /// When no repository is named `old`, or `new` holds no `/`.
    pub fn rename_repo(&self, old: &str, new: &str) {
        self.state().rename_repo(old, new);
    }

    /// Two releases may share a tag, as two drafts on GitHub can.
    ///
    /// # Panics
    ///
    /// When no repository is named `slug`.
    pub fn add_release(&self, slug: &str, spec: ReleaseSpec) -> u64 {
        self.state().add_release(slug, spec)
    }

    /// The asset's id. A name the release already holds is replaced in place, keeping its id.
    ///
    /// # Panics
    ///
    /// When no release has `release_id`.
    pub fn add_asset(&self, release_id: u64, spec: AssetSpec) -> u64 {
        self.state().add_asset(release_id, spec)
    }

    /// The release `releases/latest` answers, or none (a 404). Until this is called it is
    /// the newest published non-prerelease, as it is again once the set release is gone.
    ///
    /// # Panics
    ///
    /// When no repository is named `slug`.
    pub fn set_latest(&self, slug: &str, release_id: Option<u64>) {
        self.state().set_latest(slug, release_id);
    }

    /// A file at `path` under `reference` (a branch, a tag or a sha).
    ///
    /// # Panics
    ///
    /// When no repository is named `slug`.
    pub fn put_file(&self, slug: &str, reference: &str, path: &str, bytes: &[u8]) {
        self.state().put_file(slug, reference, path, bytes);
    }

    /// Whether the tree answers `truncated`.
    ///
    /// # Panics
    ///
    /// When no repository is named `slug`.
    pub fn set_tree_truncated(&self, slug: &str, truncated: bool) {
        self.state().set_tree_truncated(slug, truncated);
    }

    /// A commit, newest last.
    ///
    /// # Panics
    ///
    /// When no repository is named `slug`.
    pub fn add_commit(&self, slug: &str, spec: CommitSpec) {
        self.state().add_commit(slug, spec);
    }

    /// A branch at `sha`.
    ///
    /// # Panics
    ///
    /// When no repository is named `slug`.
    pub fn set_branch(&self, slug: &str, name: &str, sha: &str) {
        self.state().set_branch(slug, name, sha);
    }

    /// A tag at `sha`.
    ///
    /// # Panics
    ///
    /// When no repository is named `slug`.
    pub fn set_tag(&self, slug: &str, name: &str, sha: &str) {
        self.state().set_tag(slug, name, sha);
    }

    /// What `compare/{base}...{head}` answers.
    ///
    /// # Panics
    ///
    /// When no repository is named `slug`.
    pub fn set_compare(&self, slug: &str, base: &str, head: &str, spec: CompareSpec) {
        self.state().set_compare(slug, base, head, spec);
    }

    /// Apply `change` now.
    ///
    /// # Panics
    ///
    /// When the change names a release or repository the fake does not hold.
    pub fn apply(&self, change: Change) {
        self.state().apply(change);
    }

    /// Apply `change` once the `count`-th request matching `m` has been answered: the answer
    /// is decided first, and the change is in place before its bytes are written, so the
    /// client's next request sees it. A request a `Fault::Close` or `Fault::Hang` meets gets
    /// no answer, so it does not count. A repository a `SetLatest` or `Hide` names is bound
    /// now, so the change follows it through a later `rename_repo`. A change something has made
    /// stale by the time it is due (its release deleted) is skipped and recorded in
    /// `violations()`, and the answer is still written.
    ///
    /// # Panics
    ///
    /// When the change names a release or repository the fake does not hold now.
    pub fn after(&self, m: Match, count: usize, change: Change) {
        self.state().after(m, count, change);
    }

    /// The fault fires on the next matching request, once. A matching once-only fault fires
    /// before any `fault_always` one, so it is never shadowed by an overlapping always fault;
    /// of several matching once-only faults, the first added fires first.
    pub fn fault(&self, m: Match, fault: Fault) {
        self.state().rules.push(Rule {
            m,
            fault,
            always: false,
        });
    }

    /// The fault fires on every matching request that no once-only fault (`fault`) takes; of
    /// several matching always faults, the first added fires.
    pub fn fault_always(&self, m: Match, fault: Fault) {
        self.state().rules.push(Rule {
            m,
            fault,
            always: true,
        });
    }

    /// Every request so far, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        self.state().seen.clone()
    }

    /// Every breach of the wire rules so far, and every `after` change skipped as stale.
    #[must_use]
    pub fn violations(&self) -> Vec<String> {
        self.state().violations.clone()
    }

    /// No violation so far.
    ///
    /// # Panics
    ///
    /// When there is one, naming every one.
    pub fn assert_clean(&self) {
        let violations = self.violations();
        assert!(violations.is_empty(), "{violations:?}");
    }

    /// Forget every request seen.
    pub fn clear_seen(&self) {
        self.state().seen.clear();
    }
}

impl Drop for FakeGithub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // An accept loop parked in its back-off sees the flag at once.
        for accept in &self.accepts {
            accept.thread().unpark();
        }
        server::wake(self.ports);
        for accept in self.accepts.drain(..) {
            let _ = accept.join();
        }
    }
}
