// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HTTP client for routed, rate-recorded, and retried GitHub API calls.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use aterm_types::MutexExt;

use crate::classify;
use crate::config::Dirs;
use crate::error::{Error, HttpRef, IdentityRef, Kind, Replay, Result, SideEffect, Sub, Tried};
use crate::fx::{Fx, Mode, canon};
use crate::git::{GitOut, GitRun, HelperCommand};
use crate::github::{How, repo_root};
use crate::identity::{self, Credential, EnvSource, LoadError, ProcessEnv, Source, TokenKind};
use crate::journal::{IntentDraft, IntentStore, Journal, Settled};
use crate::notify::{NoNotifier, Notifier};
use crate::policy::{
    Access, AnonymousWhy, Decision, Permit, PermitTarget, Presence, Refusal, RefusalCode,
    RouteRequest, RouteTarget, Router,
};
use crate::rate::{RateBook, RateSnapshot, book_key_str, resource_for};
use crate::redact::Redactor;
use crate::retry::{Idempotency, RetryDecision, RetryPolicy, should_retry, transient_cause};
use crate::slug::Slug;
use crate::wire::door::AuthHeader;
use crate::wire::http::HttpTransport;
use crate::wire::{
    Deadlines, Endpoints, Method, Origin, OriginRole, Transport, WireBody, WireError, WireRequest,
    WireResponse, header_in, hop, path_only,
};
use crate::write::{Outcome, WriteOp};

fn retry_exhausted(mut failure: Error, bound: &str) -> Error {
    failure.why = format!("{} after {bound}", failure.why);
    failure
}

/// The Accept header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Accept {
    /// `application/vnd.github+json`.
    Json,
    /// `application/vnd.github.raw+json`.
    Raw,
    /// `application/vnd.github.sha`.
    Sha,
    /// `application/octet-stream`.
    OctetStream,
    /// Any other media type.
    Media(String),
}

impl Accept {
    /// Header value for this `Accept` media type.
    #[inline]
    #[must_use]
    pub fn header_value(&self) -> &str {
        match self {
            Accept::Json => "application/vnd.github+json",
            Accept::Raw => "application/vnd.github.raw+json",
            Accept::Sha => "application/vnd.github.sha",
            Accept::OctetStream => "application/octet-stream",
            Accept::Media(media) => media,
        }
    }
}

/// A write's body. A file body is reopened for every attempt, so a retried upload re-reads
/// its bytes from the start.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CallBody {
    /// No body.
    Empty,
    /// Compact JSON, `Content-Type: application/json`.
    Json(Vec<u8>),
    /// The file at `path`, exactly `len` bytes, with `content_type`.
    File {
        /// The file.
        path: std::path::PathBuf,
        /// Its length, which the send checks again before it streams.
        len: u64,
        /// `Content-Type`.
        content_type: String,
    },
}

const CALL_LIMIT: u64 = 32 << 20;

/// One request before routing.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Call {
    /// The method.
    pub method: Method,
    /// The origin's role.
    pub origin: OriginRole,
    /// The path, already encoded.
    pub path: String,
    /// Raw query pairs; `path_and_query` encodes them.
    pub query: Vec<(String, String)>,
    /// The Accept header.
    pub accept: Accept,
    /// How it is routed.
    pub route: RouteRequest,
    /// Whether it may be sent again.
    pub idempotency: Idempotency,
    /// The most body bytes the answer may carry.
    pub limit: u64,
    /// The time limits.
    pub deadlines: Deadlines,
    /// `If-None-Match`.
    pub if_none_match: Option<String>,
    /// Whether a 404 is probed.
    pub probe_404: bool,
    /// Whether a 301 is followed.
    pub follow_moved: bool,
    /// Whether one redirect hop to the CDN is made (`call_stream` only).
    pub hop: bool,
    /// `None`: the client's policy.
    pub retry: Option<RetryPolicy>,
    /// Hand back any definitive status as an `Answered`, with no probe, move, or hop. A
    /// transient status still enters the call's shared retry budget first.
    pub answer_any: bool,
    /// The write's body (`CallBody::Empty` from `Call::get` and `Call::head`).
    pub(crate) body: CallBody,
    /// Whether the rate book may refuse it before it is sent (`How::rate_book`; default
    /// true). The reads that guard a write set it false (design §7.4).
    pub(crate) rate_book: bool,
    /// The most its retry sleeps may take (`How::sleep_budget`; default `None`: no cap).
    pub(crate) sleep_budget: Option<Duration>,
    /// The exact-body capability of the typed atomic-ref resource; never set
    /// by a generic call or raw API request.
    pub(crate) atomic_refs: Option<crate::github::refs::AtomicRefWrite>,
    /// A typed repository-creation call's exact-body capability.
    pub(crate) repo_create: Option<crate::github::repos::RepoCreateWrite>,
}

impl Call {
    /// Build a GET call on the API origin with standard defaults.
    pub fn get(path: impl Into<String>, route: RouteRequest) -> Call {
        let follow_moved = route.access == Access::Read;
        Call {
            method: Method::Get,
            origin: OriginRole::Api,
            path: path.into(),
            query: Vec::new(),
            accept: Accept::Json,
            route,
            idempotency: Idempotency::Read,
            limit: CALL_LIMIT,
            deadlines: Deadlines::JSON,
            if_none_match: None,
            probe_404: true,
            follow_moved,
            hop: false,
            retry: None,
            answer_any: false,
            body: CallBody::Empty,
            rate_book: true,
            sleep_budget: None,
            atomic_refs: None,
            repo_create: None,
        }
    }

    /// As `get`, with HEAD.
    pub fn head(path: impl Into<String>, route: RouteRequest) -> Call {
        Call {
            method: Method::Head,
            ..Call::get(path, route)
        }
    }

    /// A write: `method` (POST, PATCH, PUT or DELETE) to `path`, routed as the permit's write.
    /// Crate-private, because only a holder of a `Permit` may build one.
    pub(crate) fn write(
        method: Method,
        path: impl Into<String>,
        route: RouteRequest,
        idempotency: Idempotency,
        body: CallBody,
    ) -> Call {
        Call {
            method,
            idempotency,
            body,
            probe_404: false,
            follow_moved: false,
            ..Call::get(path, route)
        }
    }

    /// The same call consulting the rate book before it is sent, or not (`How::rate_book`).
    #[inline]
    #[must_use]
    pub(crate) const fn rate_book(mut self, on: bool) -> Call {
        self.rate_book = on;
        self
    }

    /// The same call with a cap on its retry sleeps (`How::sleep_budget`).
    #[inline]
    #[must_use]
    pub(crate) const fn sleep_budget(mut self, budget: Option<Duration>) -> Call {
        self.sleep_budget = budget;
        self
    }

    /// The same call on `origin`.
    #[inline]
    #[must_use]
    pub const fn origin(mut self, origin: OriginRole) -> Call {
        self.origin = origin;
        self
    }

    /// The same call with one more query pair.
    #[inline]
    #[must_use]
    pub fn query(mut self, key: &str, value: impl Into<String>) -> Call {
        self.query.push((key.to_string(), value.into()));
        self
    }

    /// The same call with `accept`.
    #[inline]
    #[must_use]
    pub fn accept(mut self, accept: Accept) -> Call {
        self.accept = accept;
        self
    }

    /// The same call with `limit`.
    #[inline]
    #[must_use]
    pub const fn limit(mut self, limit: u64) -> Call {
        self.limit = limit;
        self
    }

    /// The same call with `deadlines`.
    #[inline]
    #[must_use]
    pub const fn deadlines(mut self, deadlines: Deadlines) -> Call {
        self.deadlines = deadlines;
        self
    }

    /// The same call with `If-None-Match`.
    #[inline]
    #[must_use]
    pub fn if_none_match(mut self, etag: Option<String>) -> Call {
        self.if_none_match = etag;
        self
    }

    /// The same call with the 404 probe on or off.
    #[inline]
    #[must_use]
    pub const fn probe_404(mut self, probe: bool) -> Call {
        self.probe_404 = probe;
        self
    }

    /// The same call following a 301 or not.
    #[inline]
    #[must_use]
    pub const fn follow_moved(mut self, follow: bool) -> Call {
        self.follow_moved = follow;
        self
    }

    /// The same call hopping or not (for `call_stream`; see the field).
    #[inline]
    #[must_use]
    pub const fn hop(mut self, hop: bool) -> Call {
        self.hop = hop;
        self
    }

    /// The same call with its own retry policy.
    #[inline]
    #[must_use]
    pub const fn retry(mut self, policy: Option<RetryPolicy>) -> Call {
        self.retry = policy;
        self
    }

    /// The same call answering any status or not.
    #[inline]
    #[must_use]
    pub const fn answer_any(mut self, any: bool) -> Call {
        self.answer_any = any;
        self
    }

    /// Encoded path and query string, borrowing `self.path` when `self.query` is empty.
    #[must_use]
    pub(crate) fn path_and_query_cow(&self) -> std::borrow::Cow<'_, str> {
        if self.query.is_empty() {
            std::borrow::Cow::Borrowed(&self.path)
        } else {
            let join = if self.path.contains('?') { '&' } else { '?' };
            let cap = self.path.len()
                + 1
                + self
                    .query
                    .iter()
                    .map(|(k, v)| k.len() + v.len() + 2)
                    .sum::<usize>();
            let mut out = String::with_capacity(cap);
            out.push_str(&self.path);
            out.push(join);
            crate::path::query_into(&mut out, &self.query);
            std::borrow::Cow::Owned(out)
        }
    }

    /// Encoded path and query string.
    #[must_use]
    pub fn path_and_query(&self) -> String {
        self.path_and_query_cow().into_owned()
    }
}

/// A whole answer.
#[derive(Clone, Debug)]
pub struct Answered {
    /// The status.
    pub status: u16,
    /// The kept headers, in received order.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
    /// Who it was read as.
    pub as_: IdentityRef,
    /// `X-GitHub-Request-Id`.
    pub request_id: Option<String>,
    /// `ETag`.
    pub etag: Option<String>,
    /// `(old, new)` when a 301 was followed.
    pub moved: Option<(Slug, Slug)>,
    /// Requests sent for this answer (retries, the probe and the move included).
    pub requests: u32,
    /// The answer's rate-limit snapshot.
    pub rate: Option<RateSnapshot>,
}

impl Answered {
    /// The body as JSON.
    /// # Errors
    /// CouldNotRun, `Sub::Protocol`, why `GitHub's answer is not JSON: {e}`.
    pub fn json(&self) -> Result<aterm_json::Value> {
        aterm_json::from_slice(&self.body)
            .map_err(|e| Error::protocol(format!("GitHub's answer is not JSON: {e}")))
    }

    /// The first header so named, ASCII case-insensitively.
    #[inline]
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header_in(&self.headers, name)
    }
}

/// A 2xx answer whose body is read as it streams.
pub struct Streamed {
    /// The status.
    pub status: u16,
    /// The kept headers, in received order.
    pub headers: Vec<(String, String)>,
    /// The body, unread.
    pub body: Box<dyn std::io::Read + Send>,
    /// Who it was read as.
    pub as_: IdentityRef,
    /// Requests sent for this answer.
    pub requests: u32,
    /// Whether a hop was made.
    pub hopped: bool,
}

impl Streamed {
    /// The first header so named, ASCII case-insensitively.
    #[inline]
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header_in(&self.headers, name)
    }
}

impl std::fmt::Debug for Streamed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Streamed")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("as_", &self.as_)
            .field("requests", &self.requests)
            .field("hopped", &self.hopped)
            .finish_non_exhaustive()
    }
}

/// Client request telemetry read by the CLI receipt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meter {
    /// Requests sent (retries, probes, moves and hops included).
    pub requests: u32,
    /// REST requests by identity, including retries and probes; excludes web/CDN transfers.
    pub rest_requests: BTreeMap<String, u32>,
    /// The identity of the last request sent.
    pub as_: Option<IdentityRef>,
    /// The last `x-ratelimit-*` snapshot.
    pub rate: Option<RateSnapshot>,
    /// `(old, new)` once a 301 was followed.
    pub moved: Option<(Slug, Slug)>,
    /// Whether a hop was made.
    pub hopped: bool,
    /// Whether a read went anonymous without `--anonymous`.
    pub fell_to_anonymous: bool,
    /// The last `--notify` post's `Posted::receipt`, which the receipt prints as the note
    /// `notify={value}`.
    pub notify: Option<String>,
}

impl Meter {
    /// The last request's identity as a label.
    #[inline]
    pub(crate) fn label(&self) -> &str {
        self.as_.as_ref().map_or("anonymous", IdentityRef::label)
    }

    /// Record one REST request sent as `as_` without allocating a key when already present.
    pub(crate) fn note_rest_request(&mut self, as_: &IdentityRef) {
        let label = as_.label();
        if let Some(count) = self.rest_requests.get_mut(label) {
            *count = count.saturating_add(1);
        } else {
            self.rest_requests.insert(label.to_string(), 1);
        }
    }
}

/// The login bindings of a client's inline profiles (design §6.1).
#[derive(Default)]
struct Bindings {
    /// Each inline profile a binding began for: `false` while its `GET /user` runs, `true`
    /// once its login matched.
    profiles: BTreeMap<String, bool>,
    /// The sends waiting now for another send's binding to end: a test's reading only.
    #[cfg(any(test, debug_assertions))]
    waiting: usize,
}

/// Ends a binding that did not bind (an error, a mismatch, or a panic in its read): its
/// entry is removed and every waiting send wakes, to bind again itself.
struct Binder<'a> {
    gh: &'a Github,
    profile: &'a str,
    bound: bool,
}

impl Drop for Binder<'_> {
    fn drop(&mut self) {
        let mut held = self.gh.inner.bound.lock_or_recover();
        if self.bound {
            held.profiles.insert(self.profile.to_string(), true);
        } else {
            held.profiles.remove(self.profile);
        }
        drop(held);
        self.gh.inner.bind_done.notify_all();
    }
}

struct Inner {
    router: Router,
    transport: Arc<dyn Transport>,
    endpoints: Endpoints,
    env: Arc<dyn EnvSource + Send + Sync>,
    rate_book: RateBook,
    fx: Arc<Fx>,
    caller: String,
    user_agent: String,
    retry: RetryPolicy,
    redactor: Redactor,
    backstop: Option<Duration>,
    no_anonymous_api: bool,
    /// Each profile's load, once per process.
    credentials: Mutex<BTreeMap<String, std::result::Result<Arc<Credential>, LoadError>>>,
    /// Each repository's numeric id, by [`Slug`]: the only memo (design §14).
    ids: Mutex<BTreeMap<Slug, u64>>,
    meter: Mutex<Meter>,
    /// The inline profiles whose login a `GET /user` bound (design §6.1), or is binding now.
    bound: Mutex<Bindings>,
    /// Wakes the sends that wait on another send's binding.
    bind_done: Condvar,
    /// `fx.mono()` when this client's last write attempt began: the pacing clock (design §7.4).
    last_write: Mutex<Option<Duration>>,
    /// The directories `GithubBuilder::journal` set: the write journals, grants and needs.
    dirs: Option<Dirs>,
    /// Where `How::notify` posts go.
    notifier: Arc<dyn Notifier>,
    /// The git helper command `git_helper_for_tests` set.
    git_helper: Option<HelperCommand>,
    /// The local repositories `git_remote_for_tests` set, by `Slug::key`.
    #[cfg(any(test, debug_assertions))]
    git_remotes: BTreeMap<String, std::path::PathBuf>,
}

/// The client. A cheap `Clone` over one `Arc`; `Send + Sync`.
#[derive(Clone)]
pub struct Github {
    inner: Arc<Inner>,
}

/// Builds a `Github`.
pub struct GithubBuilder {
    router: Router,
    transport: Option<Arc<dyn Transport>>,
    endpoints: Endpoints,
    env: Arc<dyn EnvSource + Send + Sync>,
    rate_book: RateBook,
    fx: Option<Arc<Fx>>,
    caller: String,
    retry: RetryPolicy,
    redactor: Redactor,
    backstop: Option<Duration>,
    no_anonymous_api: bool,
    dirs: Option<Dirs>,
    notifier: Arc<dyn Notifier>,
    git_helper: Option<HelperCommand>,
    #[cfg(any(test, debug_assertions))]
    git_remotes: BTreeMap<String, std::path::PathBuf>,
}

impl GithubBuilder {
    /// Create a builder with default transport, endpoints, env, rate book, fx, and retry policy.
    #[must_use]
    pub fn new(router: Router) -> GithubBuilder {
        GithubBuilder {
            router,
            transport: None,
            endpoints: Endpoints::github_com(),
            env: Arc::new(ProcessEnv),
            rate_book: RateBook::none(),
            fx: None,
            caller: "lib".to_string(),
            retry: RetryPolicy::STANDARD,
            redactor: Redactor::new(),
            backstop: None,
            no_anonymous_api: false,
            dirs: None,
            notifier: Arc::new(NoNotifier),
            git_helper: None,
            #[cfg(any(test, debug_assertions))]
            git_remotes: BTreeMap::new(),
        }
    }

    /// Refuse anonymous API sends before the wire, including an absent reader's fallback.
    /// Credential-free Git and web/CDN transfers do not consume this API budget.
    #[must_use]
    pub fn refuse_anonymous_api(mut self, refuse: bool) -> GithubBuilder {
        self.no_anonymous_api = refuse;
        self
    }

    /// Set a custom sealed transport.
    #[must_use]
    pub fn transport(mut self, transport: Arc<dyn Transport>) -> GithubBuilder {
        self.transport = Some(transport);
        self
    }

    /// Set the target endpoints.
    #[must_use]
    pub fn endpoints(mut self, endpoints: Endpoints) -> GithubBuilder {
        self.endpoints = endpoints;
        self
    }

    /// The environment: proxy variables, env profiles, ambient tokens.
    #[must_use]
    pub fn env(mut self, env: Arc<dyn EnvSource + Send + Sync>) -> GithubBuilder {
        self.env = env;
        self
    }

    /// The rate book.
    #[must_use]
    pub fn rate_book(mut self, book: RateBook) -> GithubBuilder {
        self.rate_book = book;
        self
    }

    /// Set the effect seam for clock reads, sleeps, and round trips.
    #[must_use]
    pub fn fx(mut self, fx: Arc<Fx>) -> GithubBuilder {
        self.fx = Some(fx);
        self
    }

    /// The `User-Agent` caller: `agh/{version} ({caller})`.
    #[must_use]
    pub fn caller(mut self, caller: &str) -> GithubBuilder {
        self.caller = caller.to_string();
        self
    }

    /// The retry policy.
    #[must_use]
    pub const fn retry(mut self, policy: RetryPolicy) -> GithubBuilder {
        self.retry = policy;
        self
    }

    /// The redactor the client teaches every credential it loads.
    #[must_use]
    pub fn redactor(mut self, redactor: Redactor) -> GithubBuilder {
        self.redactor = redactor;
        self
    }

    /// Every call's backstop, when set (`--timeout`): replaces `call.deadlines.backstop`.
    #[must_use]
    pub const fn backstop(mut self, backstop: Option<Duration>) -> GithubBuilder {
        self.backstop = backstop;
        self
    }

    /// Where the write journals, grants and need files live: `dirs.journal()`, `dirs.grants()`
    /// and `dirs.needs()`. Without it a one-shot write is refused, and so is every git run
    /// that needs a grant: a release-tier run and every `PushDelete`. The runner also hands
    /// these directories to the git helper.
    #[must_use]
    pub fn journal(mut self, dirs: Dirs) -> GithubBuilder {
        self.dirs = Some(dirs);
        self
    }

    /// The notifier for `How::notify` (default `NoNotifier`).
    #[must_use]
    pub fn notifier(mut self, notifier: Arc<dyn Notifier>) -> GithubBuilder {
        self.notifier = notifier;
        self
    }

    /// Tests: the git helper command the runner mounts (default `HelperCommand::current()`).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn git_helper_for_tests(mut self, helper: HelperCommand) -> GithubBuilder {
        self.git_helper = Some(helper);
        self
    }

    /// Tests: `slug`'s git URL is the local repository at `path`, and that run alone sets
    /// `protocol.file.allow=always`.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn git_remote_for_tests(mut self, slug: &str, path: &std::path::Path) -> GithubBuilder {
        self.git_remotes
            .insert(slug.to_ascii_lowercase(), path.to_path_buf());
        self
    }

    /// The client.
    #[must_use]
    pub fn build(self) -> Github {
        let transport = self
            .transport
            .unwrap_or_else(|| Arc::new(HttpTransport::new(self.env.clone())));
        let user_agent = format!("agh/{} ({})", crate::version(), self.caller);
        Github {
            inner: Arc::new(Inner {
                router: self.router,
                transport,
                endpoints: self.endpoints,
                env: self.env,
                rate_book: self.rate_book,
                fx: self.fx.unwrap_or_else(|| Arc::new(Fx::live())),
                caller: self.caller,
                user_agent,
                retry: self.retry,
                redactor: self.redactor,
                backstop: self.backstop,
                no_anonymous_api: self.no_anonymous_api,
                credentials: Mutex::new(BTreeMap::new()),
                ids: Mutex::new(BTreeMap::new()),
                meter: Mutex::new(Meter::default()),
                bound: Mutex::new(Bindings::default()),
                bind_done: Condvar::new(),
                last_write: Mutex::new(None),
                dirs: self.dirs,
                notifier: self.notifier,
                git_helper: self.git_helper,
                #[cfg(any(test, debug_assertions))]
                git_remotes: self.git_remotes,
            }),
        }
    }
}

impl Github {
    /// Execute a call and read its body whole.
    /// # Errors
    /// The routed, classified error. In M1, a `call.method` other than GET and HEAD is Refused
    /// `policy.method_not_read` before anything is routed, loaded or sent (plan §8, step 0). A call
    /// with `hop` set is CouldNotRun `Protocol`, why `a hop is followed only by a streamed call`,
    /// also before anything is routed: `Fx::gh` rewrites a CDN `Location`, so the hop could only
    /// fail. The rate book is read and written for api-origin calls only (step 2): the web and CDN
    /// origins do not spend the API budget.
    pub fn call(&self, call: &Call) -> Result<Answered> {
        method_gate(call)?;
        if call.hop {
            let error = Error::protocol("a hop is followed only by a streamed call");
            return Err(match repo_of(&call.route) {
                Some(slug) => error.with_repo(slug.to_string()),
                None => error,
            });
        }
        let decided = self.route(&call.route)?;
        self.run(call, &decided, Reading::Whole)
            .and_then(Got::into_answered)
    }

    /// Execute a call and stream its 2xx body.
    /// # Errors
    /// As `call`.
    pub fn call_stream(&self, call: &Call) -> Result<Streamed> {
        method_gate(call)?;
        let decided = self.route(&call.route)?;
        self.run(call, &decided, Reading::Stream)
            .map(Got::into_streamed)
    }

    /// Execute a file transfer whose completed prefix earns a bounded Range resume.
    /// `offset` is the number of durable bytes already held by the caller; `consume` must
    /// append a validated 206 body or replace the prefix when the server answers 200.
    pub(crate) fn call_stream_resumable_with<T>(
        &self,
        call: &Call,
        mut offset: impl FnMut() -> u64,
        mut consume: impl FnMut(Streamed) -> Result<T>,
    ) -> Result<T> {
        method_gate(call)?;
        let decided = self.route(&call.route)?;
        let mut sent = 0;
        let mut resume = ProgressResume::new(call.limit, &mut offset);
        self.run_counted_with(
            call,
            &decided,
            Reading::Stream,
            &mut sent,
            Some(&mut resume),
            &mut |got| consume(got.into_streamed()),
        )
    }

    /// The router.
    #[inline]
    #[must_use]
    pub fn router(&self) -> &Router {
        &self.inner.router
    }

    /// The endpoints.
    #[inline]
    #[must_use]
    pub fn endpoints(&self) -> &Endpoints {
        &self.inner.endpoints
    }

    /// The redactor.
    #[inline]
    #[must_use]
    pub fn redactor(&self) -> &Redactor {
        &self.inner.redactor
    }

    /// `agh/{version} ({caller})`.
    #[inline]
    #[must_use]
    pub fn user_agent(&self) -> &str {
        &self.inner.user_agent
    }

    /// The environment.
    #[inline]
    #[must_use]
    pub fn env(&self) -> &(dyn EnvSource + Send + Sync) {
        &*self.inner.env
    }

    /// The client's effect seam: waits and the receipt's timing read it.
    #[inline]
    #[must_use]
    pub fn fx(&self) -> &Arc<Fx> {
        &self.inner.fx
    }

    /// The rate book.
    #[inline]
    #[must_use]
    pub fn rate_book(&self) -> &RateBook {
        &self.inner.rate_book
    }

    /// The retry policy.
    #[inline]
    #[must_use]
    pub fn retry_policy(&self) -> RetryPolicy {
        self.inner.retry
    }

    /// A copy of the meter.
    #[must_use]
    pub fn meter(&self) -> Meter {
        self.inner
            .meter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Load (once per process, then cached) and judge a profile.
    #[must_use]
    pub fn presence(&self, profile: &str) -> Presence {
        let Some(row) = self.inner.router.profile(profile) else {
            return Presence::Absent;
        };
        let mut credentials = self
            .inner
            .credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(loaded) = credentials.get(profile) {
            return match loaded {
                Ok(_) => Presence::Present,
                Err(e) => e.presence(),
            };
        }
        let loaded = identity::load(row, &*self.inner.env).map(Arc::new);
        if let Ok(cred) = &loaded {
            self.inner.redactor.learn(cred.expose());
        }
        let presence = match &loaded {
            Ok(_) => Presence::Present,
            Err(e) => e.presence(),
        };
        credentials.insert(profile.to_string(), loaded);
        presence
    }

    /// Whether the profile's cached credential vouches for draft access.
    #[must_use]
    pub fn drafts_vouched(&self, profile: &str) -> bool {
        if self.presence(profile) != Presence::Present {
            return false;
        }
        let Some(row) = self.inner.router.profile(profile) else {
            return false;
        };
        self.credential(profile)
            .is_some_and(|cred| row.drafts_vouched(&cred))
    }

    /// `router().decide(route, &|p| self.presence(p))`.
    #[must_use]
    pub fn decide(&self, route: &RouteRequest) -> Decision {
        self.inner.router.decide(route, &|p| self.presence(p))
    }

    /// Return the repository's numeric ID from the memo or via `GET /repos/{o}/{r}`.
    /// # Errors
    /// That request's error; CouldNotRun `Protocol` when its answer holds no numeric `id`.
    pub fn repo_id(&self, slug: &Slug, route: &RouteRequest) -> Result<u64> {
        self.repo_id_counted(slug, route).map(|(id, _)| id)
    }

    /// `repo_id`, with the requests it sent (0 from the memo).
    pub(crate) fn repo_id_counted(&self, slug: &Slug, route: &RouteRequest) -> Result<(u64, u32)> {
        if let Some(id) = self
            .inner
            .ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(slug)
        {
            return Ok((*id, 0));
        }
        let mut route = route.clone();
        route.target = RouteTarget::Repo(slug.clone());
        let answered = self.call(&Call::get(repo_root(slug), route))?;
        let id = answered
            .json()
            .map_err(|error| error.with_repo(slug.to_string()))?
            .get("id")
            .and_then(aterm_json::Value::as_u64)
            .ok_or_else(|| {
                Error::protocol(format!("GitHub's answer for {slug} holds no numeric id"))
                    .with_repo(slug.to_string())
            })?;
        self.remember_repo_id(slug, id);
        Ok((id, answered.requests))
    }

    /// Memoize an id a resource read.
    pub(crate) fn remember_repo_id(&self, slug: &Slug, id: u64) {
        self.inner
            .ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(slug.clone(), id);
    }

    /// Count in this client's meter the requests `spent` sent before it was built (vault
    /// adoption's `GET /user`, design §6.10), so the call's receipt counts them: their number
    /// is added, and their identity and rate snapshot stand until a request of this client's.
    pub(crate) fn absorb(&self, spent: &Meter) {
        if spent.requests == 0 {
            return;
        }
        let mut meter = self.inner.meter.lock_or_recover();
        meter.requests = meter.requests.saturating_add(spent.requests);
        for (identity, count) in &spent.rest_requests {
            let entry = meter.rest_requests.entry(identity.clone()).or_default();
            *entry = entry.saturating_add(*count);
        }
        if meter.as_.is_none() {
            meter.as_.clone_from(&spent.as_);
        }
        if meter.rate.is_none() {
            meter.rate.clone_from(&spent.rate);
        }
    }

    /// Send exactly `cred` as its profile (`Credential::profile`) for the rest of this
    /// client's life, in place of the lazy load of the profile's file: vault adoption judged
    /// the rows it derived by this token (design §6.10), so a file rewritten since then never
    /// speaks for them. The redactor learns the token.
    pub(crate) fn pin(&self, cred: Arc<Credential>) {
        self.inner.redactor.learn(cred.expose());
        self.inner
            .credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(cred.profile().to_string(), Ok(cred));
    }

    /// Clone client configuration over another router with fresh caches and meter.
    #[must_use]
    pub fn with_router(&self, router: Router) -> Github {
        let inner = &self.inner;
        Github {
            inner: Arc::new(Inner {
                router,
                transport: inner.transport.clone(),
                endpoints: inner.endpoints.clone(),
                env: inner.env.clone(),
                rate_book: inner.rate_book.clone(),
                fx: inner.fx.clone(),
                caller: inner.caller.clone(),
                user_agent: inner.user_agent.clone(),
                retry: inner.retry,
                redactor: inner.redactor.clone(),
                backstop: inner.backstop,
                no_anonymous_api: inner.no_anonymous_api,
                credentials: Mutex::new(BTreeMap::new()),
                ids: Mutex::new(BTreeMap::new()),
                meter: Mutex::new(Meter::default()),
                bound: Mutex::new(Bindings::default()),
                bind_done: Condvar::new(),
                last_write: Mutex::new(None),
                dirs: inner.dirs.clone(),
                notifier: inner.notifier.clone(),
                git_helper: inner.git_helper.clone(),
                #[cfg(any(test, debug_assertions))]
                git_remotes: inner.git_remotes.clone(),
            }),
        }
    }
}

impl Github {
    /// A write permit for `req`: the decision `Fx::route` gives (live, `self.decide(req)`;
    /// under a replay, the taped one), then `Router::permit_decided`. Every resource write
    /// method takes the permit its caller got here, so one write is routed once (design §10.2).
    ///
    /// # Errors
    ///
    /// As `Router::permit`; under a replay, the route door's `Replay` errors.
    pub fn permit(&self, req: &RouteRequest) -> Result<Permit> {
        let decision = self.inner.fx.route(req, &mut || self.decide(req))?;
        self.inner.router.permit_decided(req, decision)
    }

    /// Run a write `call` under `permit`: the write pipeline (design §7.4, §10.1).
    ///
    /// # Errors
    ///
    /// The pipeline's errors, each saying what the write may have done (`side_effect`).
    pub(crate) fn call_write(&self, call: &Call, permit: &Permit, how: &How) -> Result<Answered> {
        self.call_write_guarded(call, permit, how, &mut |_| Ok(None))
    }

    /// [`Github::call_write`] whose `guard` runs before every attempt: `Ok(Some(answered))`
    /// ends the loop with that answer (the guard found the state already right), `Ok(None)`
    /// sends, `Err` stops.
    ///
    /// # Errors
    ///
    /// As `call_write`; the guard's error.
    pub(crate) fn call_write_guarded(
        &self,
        call: &Call,
        permit: &Permit,
        how: &How,
        guard: &mut dyn FnMut(u32) -> Result<Option<Answered>>,
    ) -> Result<Answered> {
        let on_repo = |error: Error| with_call_repo(call, error);
        if call.method.is_read() {
            return Err(on_repo(Error::usage(format!(
                "a write method sends a write; {} is a read",
                call.method.as_str()
            ))));
        }
        write_target(call, permit).map_err(on_repo)?;
        let decided = self.decided_for(permit).map_err(on_repo)?;
        self.bind_login(permit.profile())
            .map_err(|error| on_repo(before_the_write(error)))?;
        let policy = how.retry.or(call.retry).unwrap_or(self.inner.retry);
        let budget = how.sleep_budget.or(call.sleep_budget);
        let consult_book = how.rate_book && call.rate_book;
        let mut slept = Duration::ZERO;
        let mut retry_budget = policy.budget();
        let mut attempt = 1u32;
        // What the attempts so far may have done: an error after a retry says at least that,
        // so a later attempt that sent nothing never hides an earlier one that may have landed.
        let mut earlier = SideEffect::None;
        loop {
            if let Some(answered) = guard(attempt).map_err(|e| at_least(e, earlier))? {
                return Ok(answered);
            }
            self.pace().map_err(|e| at_least(e, earlier))?;
            let failure = match self.write_attempt(call, permit, &decided, consult_book) {
                Ok(mut answered) => {
                    answered.requests = attempt;
                    return Ok(answered);
                }
                Err(failure) => failure,
            };
            let retry = match should_retry(&mut retry_budget, call.idempotency, attempt, &failure) {
                RetryDecision::Retry(retry) => retry,
                RetryDecision::Exhausted { bound } => {
                    return Err(at_least(retry_exhausted(failure, &bound), earlier));
                }
                RetryDecision::Final => return Err(at_least(failure, earlier)),
            };
            let pause = retry.pause;
            if let Some(budget) = budget
                && slept.saturating_add(pause) > budget
            {
                let bound = format!("{}s call retry-pause bound", budget.as_secs());
                return Err(at_least(retry_exhausted(failure, &bound), earlier));
            }
            earlier = stronger(earlier, failure.side_effect);
            self.inner
                .fx
                .note_retry(retry)
                .map_err(|e| at_least(e, earlier))?;
            self.inner
                .fx
                .sleep(pause)
                .map_err(|e| at_least(e, earlier))?;
            slept = slept.saturating_add(pause);
            attempt = attempt.saturating_add(1);
        }
    }

    /// A Converging write's attempts (design §7.4): one `call_write_guarded` attempt at a
    /// time under `how`'s retry policy (the client's when it names none) and sleep budget, so
    /// what each failed attempt may have done is known between them. `guard` runs before every
    /// attempt with the `Look` it is asked for: `Ok(Some(answered))` ends the loop with that
    /// answer (the state is already right, nothing sent), `Ok(None)` sends, `Err` ends it. An
    /// attempt may have landed exactly when its error's side effect is unknown (`write_attempt`
    /// records it: sent with no answer, a 5xx, or an unreadable 2xx); the latest such error
    /// is kept to the end (`ConvergingAttempts::lost`). Resources build their own ending on what
    /// this returns: `Releases::converge` and the key writes.
    pub(crate) fn converging(
        &self,
        call: &Call,
        permit: &Permit,
        how: &How,
        guard: &mut dyn FnMut(Look) -> Result<Option<Answered>>,
    ) -> ConvergingAttempts {
        let policy = how.retry.unwrap_or_else(|| self.retry_policy());
        let once = How::write().retry_policy(RetryPolicy::NONE);
        let budget = how.sleep_budget;
        let mut slept = Duration::ZERO;
        let mut retry_budget = policy.budget();
        let mut so_far = Progress {
            lost: None,
            previous: None,
            earlier: false,
            attempt: 1,
        };
        loop {
            let look = Look {
                attempt: so_far.attempt,
                lost: so_far.lost.is_some(),
                last: false,
            };
            let mut guard_failed = false;
            let result = self.call_write_guarded(call, permit, &once, &mut |_| {
                guard(look).inspect_err(|_| guard_failed = true)
            });
            let failure = match result {
                Ok(answered) => return so_far.end(Ok(answered), Ended::Answered),
                Err(read) if guard_failed => return so_far.end(Err(read), Ended::Guard),
                Err(failure) => failure,
            };
            // The latest attempt that lost its answer is kept: a later attempt answered with a
            // 4xx, or never sent, does not undo what it may have done.
            if failure.side_effect == SideEffect::Unknown {
                so_far.lost = Some(failure.clone());
            }
            let retry = match should_retry(
                &mut retry_budget,
                Idempotency::Converging,
                so_far.attempt,
                &failure,
            ) {
                RetryDecision::Retry(retry) => retry,
                RetryDecision::Exhausted { bound } => {
                    return so_far.end(Err(retry_exhausted(failure, &bound)), Ended::Attempt);
                }
                RetryDecision::Final => return so_far.end(Err(failure), Ended::Attempt),
            };
            let pause = retry.pause;
            if let Some(budget) = budget
                && slept.saturating_add(pause) > budget
            {
                let bound = format!("{}s call retry-pause bound", budget.as_secs());
                return so_far.end(Err(retry_exhausted(failure, &bound)), Ended::Attempt);
            }
            so_far.earlier |= failure.side_effect == SideEffect::Unknown;
            so_far.previous = Some(failure);
            if let Err(logged) = self.inner.fx.note_retry(retry) {
                return so_far.end(Err(logged), Ended::Pause);
            }
            if let Err(paused) = self.inner.fx.sleep(pause) {
                return so_far.end(Err(paused), Ended::Pause);
            }
            slept = slept.saturating_add(pause);
            so_far.attempt = so_far.attempt.saturating_add(1);
        }
    }

    /// The journal's draft for a one-shot `call` under `permit`: `IntentDraft::new` with the
    /// loaded credential's fingerprint (`identity::fingerprint`; `taped` under any tape), then
    /// `request` set to `fx::canon::request_digest` of the call's canonical credentialed bytes
    /// (`canon::request_credentialed`). The resource adds the target and the body digest.
    ///
    /// # Errors
    ///
    /// Unauthenticated when, with no tape, the permit's profile gives no credential; as
    /// `request_credentialed`.
    pub(crate) fn intent_draft(
        &self,
        op: WriteOp,
        slug: &Slug,
        call: &Call,
        permit: &Permit,
        key: &str,
    ) -> Result<IntentDraft> {
        let decided = self.decided_for(permit)?;
        let fingerprint = if self.taped() {
            TAPED.to_string()
        } else {
            match &decided.cred {
                Some(cred) => cred.fingerprint().to_string(),
                None => return Err(not_loaded(permit.profile())),
            }
        };
        let prepared = self.prepare_write(call, &decided);
        let mut file = None;
        let (body, digest) = write_body(call, &mut file, true)?;
        let request = prepared.write_request(permit, body, digest.as_deref());
        let canonical = canon::request_credentialed(&request, &self.inner.endpoints)?;
        Ok(IntentDraft::new(op, slug, permit, &fingerprint, key)
            .with_request(canon::request_digest(&canonical)))
    }

    /// One one-shot write (design §10.2): first the call's target against the permit's
    /// (`write_target`) and the login binding, so neither failure leaves an intent; then
    /// `intents.latest(draft.op, &draft.key)`, whose id becomes the draft's `after`, then
    /// `intents.begin(draft)`, `call_write` once, then `intents.settle` with
    /// `write::settle_table` of the result and `created(&answered)`, an `Unknown`'s why and a
    /// `Created`'s url passed through `self.redactor().apply` first, so a learned secret of
    /// no known shape never reaches the journal. `NeverSent` answers `Outcome::NeverSent`,
    /// `Created` answers `Outcome::Done`, `Refused` answers `Outcome::Refused`, and `Unknown`
    /// answers `Outcome::Unknown` with the intent. For an `Ok` with no object id there is no
    /// wire error, so `one_shot` makes the `Unknown`'s error itself, with
    /// `.with_repo(draft.slug.to_string())`, so `Outcome::into_result`'s next names the repo.
    /// `Refs::create` and `Raw::send` use it; `Releases::create` runs the same steps one by
    /// one, because AghEnsureOnce binds each.
    ///
    /// # Errors
    ///
    /// `write_target`'s Usage and the login binding's error, with no intent begun.
    /// `begin`'s refusal, and nothing is sent. A `settle` that fails after GitHub answered is
    /// could_not_run, `Sub::Local`, `side_effect` applied for a 2xx and unknown otherwise, why
    /// `GitHub answered {status}, and the journal could not record it: {e}; intent {id} stays
    /// open`, next `agh intent show {repo} {id}`. The open intent then stops a second POST of
    /// the same op and key at `begin` (`IntentStore::begin`), until `agh intent retract`
    /// closes it. A `RefCreate` is the one exception: its second POST is GitHub's 422
    /// `Reference already exists`, which `Refs::create` resolves by reading the ref.
    pub(crate) fn one_shot(
        &self,
        call: &Call,
        permit: &Permit,
        draft: &IntentDraft,
        intents: &dyn IntentStore,
        created: &dyn Fn(&Answered) -> Option<(u64, String)>,
    ) -> Result<Outcome<Answered>> {
        self.one_shot_inner(call, permit, draft, intents, created, false)
    }

    /// As `one_shot`, retaining the resource's already-judged `draft.after`.
    /// A concurrent intent must refuse at begin, never become a new baseline
    /// after the resource's existence and recovery checks have run.
    pub(crate) fn one_shot_judged(
        &self,
        call: &Call,
        permit: &Permit,
        draft: &IntentDraft,
        intents: &dyn IntentStore,
        created: &dyn Fn(&Answered) -> Option<(u64, String)>,
    ) -> Result<Outcome<Answered>> {
        self.one_shot_inner(call, permit, draft, intents, created, true)
    }

    fn one_shot_inner(
        &self,
        call: &Call,
        permit: &Permit,
        draft: &IntentDraft,
        intents: &dyn IntentStore,
        created: &dyn Fn(&Answered) -> Option<(u64, String)>,
        judged: bool,
    ) -> Result<Outcome<Answered>> {
        // The target and the login binding come before the journal: a call its permit does
        // not cover, or a binding that fails, leaves no intent in it.
        let on_repo = |error: Error| with_call_repo(call, error);
        write_target(call, permit).map_err(on_repo)?;
        self.bind_login(permit.profile())
            .map_err(|error| on_repo(before_the_write(error)))?;
        let after = if judged {
            draft.after.clone()
        } else {
            intents
                .latest(draft.op, &draft.key)?
                .map(|view| view.intent.id)
        };
        let id = intents.begin(&draft.clone().with_after(after))?;
        let result = self.call_write(call, permit, &How::write());
        let made = result.as_ref().ok().and_then(created);
        let settled = match crate::write::settle_table(&result, made) {
            Settled::Unknown { why } => Settled::Unknown {
                why: self.redactor().apply(&why),
            },
            Settled::Created { id, url } => Settled::Created {
                id,
                url: self.redactor().apply(&url),
            },
            settled => settled,
        };
        if let Err(e) = intents.settle(&id, &settled) {
            let (status, side_effect) = match &result {
                Ok(answered) => (answered.status.to_string(), SideEffect::Applied),
                Err(error) => (
                    error
                        .http
                        .as_ref()
                        .map_or_else(|| "nothing".to_string(), |http| http.status.to_string()),
                    SideEffect::Unknown,
                ),
            };
            let why = format!(
                "GitHub answered {status}, and the journal could not record it: {}; intent {id} stays open",
                e.why
            );
            return Err(Error::local(self.redactor().apply(&why))
                .with_side_effect(side_effect)
                .with_next(format!("agh intent show {} {id}", draft.slug))
                .with_repo(draft.slug.to_string()));
        }
        Ok(match (settled, result) {
            (Settled::Created { .. }, Ok(answered)) => Outcome::Done(answered),
            (Settled::Unknown { why }, Ok(_)) => Outcome::Unknown {
                intent: id,
                error: Error::new(Kind::OutcomeUnknown, why)
                    .with_side_effect(SideEffect::Applied)
                    .with_repo(draft.slug.to_string()),
            },
            (Settled::NeverSent, Err(error)) => Outcome::NeverSent(error),
            (Settled::Refused { .. }, Err(error)) => Outcome::Refused(error),
            (_, Err(error)) => Outcome::Unknown { intent: id, error },
            // `settle_table` answers an `Ok` with `Created` or `Unknown` alone.
            (_, Ok(answered)) => Outcome::Done(answered),
        })
    }

    /// The isolated git runner (design §6.9).
    ///
    /// # Errors
    ///
    /// As `git::run`.
    pub fn run_git(&self, run: &GitRun) -> Result<GitOut> {
        if matches!(
            run.op,
            crate::git::GitOp::PushControl { dry_run: false, .. }
        ) {
            return Err(Error::usage(
                "a control transaction requires the journaled refs resource boundary",
            ));
        }
        crate::git::run(self, run)
    }

    /// One exact native ref push, using the same repository-wide journal bucket
    /// as the API batch. Preparation failures close with no remote side effect;
    /// an ambiguous push keeps the intent open and cannot be retried silently.
    pub(crate) fn push_git_once(
        &self,
        run: &GitRun,
        permit: &Permit,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<GitOut>> {
        crate::github::permit_covers(permit, &run.slug, "refs")?;
        self.bind_login(permit.profile())
            .map_err(before_the_write)?;
        let decided = self.decided_for(permit)?;
        let fingerprint = if self.taped() {
            TAPED.to_string()
        } else {
            decided
                .cred
                .as_ref()
                .ok_or_else(|| not_loaded(permit.profile()))?
                .fingerprint()
                .to_string()
        };
        let canonical = canon::git_request(run);
        let draft = IntentDraft::new(
            WriteOp::RefUpdateAtomic,
            &run.slug,
            permit,
            &fingerprint,
            crate::github::refs::ATOMIC_KEY,
        )
        .with_request(canon::request_digest(&canonical))
        .with_body_digest(Some(crate::model::Sha256Digest::of(&canonical).to_string()))
        .with_after(
            intents
                .latest(WriteOp::RefUpdateAtomic, crate::github::refs::ATOMIC_KEY)?
                .map(|v| v.intent.id),
        );
        let id = intents.begin(&draft)?;
        // This private entry is reached only after the durable intent begins.
        // Public run_git refuses unjournaled mutable control transactions.
        let result = crate::git::run(self, run);
        let settled = match &result {
            Ok(_) => Settled::Created {
                id: 0,
                url: String::new(),
            },
            Err(error) => crate::write::settle_error(error),
        };
        let settled = match settled {
            Settled::Unknown { why } => Settled::Unknown {
                why: self.redactor().apply(&why),
            },
            other => other,
        };
        if let Err(error) = intents.settle(&id, &settled) {
            let effect = if result.is_ok() {
                SideEffect::Applied
            } else {
                SideEffect::Unknown
            };
            return Err(Error::local(format!(
                "the native push journal could not record its result: {}; intent {id} stays open",
                self.redactor().apply(&error.why)
            ))
            .with_side_effect(effect)
            .with_repo(run.slug.to_string())
            .with_next(format!("agh intent show {} {id}", run.slug)));
        }
        Ok(match result {
            Ok(out) => Outcome::Done(out),
            Err(error) => match settled {
                Settled::NeverSent => Outcome::NeverSent(error),
                Settled::Refused { .. } => Outcome::Refused(error),
                _ => Outcome::Unknown { intent: id, error },
            },
        })
    }

    /// The write journal for `slug`: `Journal::open(dirs, slug, fx)` when the builder had
    /// `journal(dirs)`; under a tape, `Journal::taped(slug, fx)` (a replay has no `Dirs`);
    /// else could_not_run `no journal: a one-shot write needs agh's state directory`.
    ///
    /// # Errors
    ///
    /// As `Journal::open`.
    pub fn journal(&self, slug: &Slug) -> Result<Journal> {
        let fx = self.fx();
        if let Some(dirs) = &self.inner.dirs {
            return Journal::open(dirs, slug, fx);
        }
        if fx.mode().is_taped() {
            return Ok(Journal::taped(slug, fx));
        }
        Err(Error::local(
            "no journal: a one-shot write needs agh's state directory",
        ))
    }

    /// The notifier `How::notify` posts through (`NoNotifier` unless the builder set one).
    #[must_use]
    pub fn notifier(&self) -> &dyn Notifier {
        &*self.inner.notifier
    }

    /// Record a `--notify` post's receipt value in the meter: `notify::settled` calls it, and
    /// `Receipt::metered` prints it as the note `notify={value}`.
    pub(crate) fn note_notify(&self, value: String) {
        self.inner.meter.lock_or_recover().notify = Some(value);
    }

    /// The directories `GithubBuilder::journal` set, if any.
    #[must_use]
    pub fn dirs(&self) -> Option<&Dirs> {
        self.inner.dirs.as_ref()
    }

    /// The helper command the runner mounts: the builder's `git_helper_for_tests`, else
    /// `HelperCommand::current()`.
    ///
    /// # Errors
    ///
    /// As `HelperCommand::current`.
    pub fn git_helper(&self) -> Result<HelperCommand> {
        match &self.inner.git_helper {
            Some(helper) => Ok(helper.clone()),
            None => HelperCommand::current(),
        }
    }

    /// Tests: the local repository `git_remote_for_tests` set for `slug`, if any.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn git_remote(&self, slug: &Slug) -> Option<&std::path::Path> {
        self.inner
            .git_remotes
            .get(&slug.key())
            .map(std::path::PathBuf::as_path)
    }
}

impl std::fmt::Debug for Github {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Github({})", self.inner.caller)
    }
}

const HEAD_MAX: u64 = 64 * 1024;

/// Whether the body is read whole inside the door (`call`) or handed back unread (`call_stream`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    Whole,
    Stream,
}

enum Body {
    Whole(Vec<u8>),
    Stream(Box<dyn Read + Send>),
}

struct Bounded {
    inner: Box<dyn Read + Send>,
    limit: u64,
    consumed: u64,
}

impl Bounded {
    fn too_large(&self) -> std::io::Error {
        std::io::Error::other(aterm_http::Error::TooLarge {
            limit: usize::try_from(self.limit).unwrap_or(usize::MAX),
        })
    }
}

impl Read for Bounded {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let room = self.limit.saturating_add(1).saturating_sub(self.consumed);
        let want = usize::try_from(room).map_or(buf.len(), |room| room.min(buf.len()));
        if want == 0 {
            return Err(self.too_large());
        }
        let read = self.inner.read(&mut buf[..want])?;
        self.consumed = self
            .consumed
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if self.consumed > self.limit {
            return Err(self.too_large());
        }
        Ok(read)
    }
}

struct Got {
    status: u16,
    headers: Vec<(String, String)>,
    body: Body,
    as_: IdentityRef,
    request_id: Option<String>,
    etag: Option<String>,
    moved: Option<(Slug, Slug)>,
    requests: u32,
    rate: Option<RateSnapshot>,
    hopped: bool,
}

impl Got {
    fn of(
        response: WireResponse,
        whole: Option<Vec<u8>>,
        as_: &IdentityRef,
        requests: u32,
        rate: Option<RateSnapshot>,
        limit: u64,
    ) -> Got {
        let WireResponse {
            status,
            headers,
            body,
        } = response;
        let body = match whole {
            Some(bytes) => Body::Whole(bytes),
            None => Body::Stream(Box::new(Bounded {
                inner: body,
                limit,
                consumed: 0,
            })),
        };
        Got {
            request_id: header_in(&headers, "x-github-request-id").map(str::to_string),
            etag: header_in(&headers, "etag").map(str::to_string),
            status,
            headers,
            body,
            as_: as_.clone(),
            moved: None,
            requests,
            rate,
            hopped: false,
        }
    }

    fn into_answered(self) -> Result<Answered> {
        let body = match self.body {
            Body::Whole(bytes) => bytes,
            Body::Stream(mut reader) => {
                let mut bytes = Vec::new();
                reader
                    .read_to_end(&mut bytes)
                    .map_err(|e| Error::local(format!("the answer could not be read: {e}")))?;
                bytes
            }
        };
        Ok(Answered {
            status: self.status,
            headers: self.headers,
            body,
            as_: self.as_,
            request_id: self.request_id,
            etag: self.etag,
            moved: self.moved,
            requests: self.requests,
            rate: self.rate,
        })
    }

    fn into_streamed(self) -> Streamed {
        let body: Box<dyn Read + Send> = match self.body {
            Body::Whole(bytes) => Box::new(std::io::Cursor::new(bytes)),
            Body::Stream(reader) => reader,
        };
        Streamed {
            status: self.status,
            headers: self.headers,
            body,
            as_: self.as_,
            requests: self.requests,
            hopped: self.hopped,
        }
    }
}

struct Decided {
    identity: IdentityRef,
    cred: Option<Arc<Credential>>,
    covers_all_repos: bool,
    /// A replay's Send: the tape decided it, no credential was loaded, and the identity's
    /// fingerprint is `taped`.
    taped: bool,
}

/// The fingerprint a taped run names in place of a credential's (plan §4.6.19).
const TAPED: &str = "taped";

/// The routing fact `decided_as` asks: whether a profile covers every repository
/// (`true` or `false`).
const COVERS: &str = "covers";

/// The pause between two write attempts of one client (design §7.4).
const PACE: Duration = Duration::from_secs(1);

struct Prepared {
    origin: OriginRole,
    atomic_refs: Option<crate::github::refs::AtomicRefWrite>,
    repo_create: Option<crate::github::repos::RepoCreateWrite>,
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    auth: Option<AuthHeader>,
    /// Who the request is sent as: the decided identity when the door gave it a credential.
    as_: IdentityRef,
    /// The profile the decision sends it as (`Decision::Send`), credential or not.
    as_profile: Option<String>,
    /// The call's limit.
    limit: u64,
    reading: Reading,
    deadlines: Deadlines,
}

const SHORT_RESUMES_PER_CALL: u32 = 16;
const RESUME_EARNED_AT: u64 = 1 << 20;

struct ProgressResume<'a> {
    cap: u64,
    offset: &'a mut dyn FnMut() -> u64,
    short: u32,
    all: u64,
}

impl<'a> ProgressResume<'a> {
    fn new(cap: u64, offset: &'a mut dyn FnMut() -> u64) -> Self {
        Self {
            cap,
            offset,
            short: 0,
            all: 0,
        }
    }

    fn begin(&mut self, prepared: &mut Prepared) -> u64 {
        let offset = (self.offset)();
        prepared
            .headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case("range"));
        if offset != 0 && offset < self.cap {
            prepared
                .headers
                .push(("Range".to_string(), format!("bytes={offset}-")));
        }
        offset
    }

    fn after(&mut self, before: u64) -> (u64, u64) {
        let after = (self.offset)();
        (after.saturating_sub(before), after)
    }

    fn admit(&mut self, moved: u64, after: u64) -> bool {
        if after >= self.cap
            || self.all >= self.budget()
            || (moved < RESUME_EARNED_AT && self.short >= SHORT_RESUMES_PER_CALL)
        {
            return false;
        }
        self.all = self.all.saturating_add(1);
        if moved < RESUME_EARNED_AT {
            self.short = self.short.saturating_add(1);
        }
        true
    }

    const fn budget(&self) -> u64 {
        (SHORT_RESUMES_PER_CALL as u64).saturating_add(self.cap / RESUME_EARNED_AT)
    }

    fn bound(&self) -> String {
        format!(
            "{} progress-earned resumes (16 short plus one per MiB of the byte cap)",
            self.budget()
        )
    }

    fn logged_retry(
        &self,
        cause: aterm_types::http_retry::Cause,
    ) -> aterm_types::http_retry::Retry {
        aterm_types::http_retry::Retry {
            attempt: u32::try_from(self.all).unwrap_or(u32::MAX),
            attempts: u32::try_from(self.budget()).unwrap_or(u32::MAX),
            cause,
            pause: Duration::ZERO,
        }
    }
}

impl Prepared {
    /// The successful streamed body's absolute byte cap. A 200 ignored Range and may replace
    /// the prefix with the whole body; a 206 may carry only the cap remaining after the exact
    /// offset we requested.
    fn stream_limit(&self, status: u16) -> u64 {
        if status != 206 {
            return self.limit;
        }
        let offset = header_in(&self.headers, "range")
            .and_then(|value| value.strip_prefix("bytes="))
            .and_then(|value| value.strip_suffix('-'))
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        self.limit.saturating_sub(offset)
    }

    fn request(&self) -> WireRequest<'_> {
        WireRequest {
            method: self.method,
            url: self.url.clone(),
            headers: self.headers.clone(),
            auth: self.auth.as_ref(),
            body: WireBody::Empty,
            limit: self.limit.max(HEAD_MAX),
            deadlines: self.deadlines,
            permit: None,
            body_digest: None,
            as_profile: self.as_profile.as_deref(),
            atomic_refs: self.atomic_refs.as_ref(),
            repo_create: self.repo_create.as_ref(),
        }
    }

    /// The write's request: `request()` carrying `permit`, `body` and the body's digest.
    fn write_request<'a>(
        &'a self,
        permit: &'a Permit,
        body: WireBody<'a>,
        body_digest: Option<&'a str>,
    ) -> WireRequest<'a> {
        WireRequest {
            body,
            permit: Some(permit),
            body_digest,
            ..self.request()
        }
    }

    fn held(&self, response: &mut WireResponse) -> std::result::Result<Option<Vec<u8>>, WireError> {
        match self.reading {
            Reading::Whole => response.read_body(self.limit).map(Some),
            Reading::Stream => Ok(None),
        }
    }
}

enum Flow {
    /// A later step (a move, a hop) produced the answer.
    Done(Box<Got>),
    /// The error, subrequests beyond the outer attempt, and whether an anonymous subread ran.
    Retry {
        error: Error,
        beyond: u32,
        anonymous: bool,
    },
}

struct Exchange<'a> {
    status: u16,
    headers: &'a [(String, String)],
    rate: Option<RateSnapshot>,
}

fn is_success(status: u16, reading: Reading) -> bool {
    (200..300).contains(&status) || (status == 304 && reading == Reading::Whole)
}

const fn repo_of(route: &RouteRequest) -> Option<&Slug> {
    match &route.target {
        RouteTarget::Repo(slug) => Some(slug),
        _ => None,
    }
}

fn protocol_error(error: Error, why: String) -> Error {
    error
        .recast(Kind::CouldNotRun, why)
        .with_sub(Sub::Protocol)
        .with_next(Kind::CouldNotRun.next_action())
}

fn after_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    crate::wire::strip_prefix_ci(path, prefix)
        .filter(|rest| rest.is_empty() || rest.starts_with(['/', '?']))
}

/// Retarget `call` to a moved repository's new slug.
pub(crate) fn retarget(call: &mut Call, old: &Slug, new: &Slug) {
    if let Some(rest) = after_prefix(&call.path, &repo_root(old)) {
        let mut path = repo_root(new);
        path.push_str(rest);
        call.path = path;
    }
    if repo_of(&call.route).is_some_and(|slug| slug == old) {
        call.route.target = RouteTarget::Repo(new.clone());
    }
}

/// The refusal of a write method where only a read is taken (`Github::call`, `Raw::get`).
pub(crate) fn method_refusal(method: Method, repo: Option<&Slug>) -> Error {
    Refusal {
        code: RefusalCode::MethodNotRead,
        why: format!(
            "{} is a write, and Github::call sends only GET and HEAD",
            method.as_str()
        ),
        next: "a write takes a permit: Router::permit, then a write method of the resource"
            .to_string(),
        tried: Vec::new(),
    }
    .into_error(repo)
}

/// What a route target is called in a refusal.
fn route_target_text(target: &RouteTarget) -> String {
    match target {
        RouteTarget::Repo(slug) => slug.to_string(),
        RouteTarget::Owner(owner) => format!("owner {owner}"),
        RouteTarget::Unscoped => "an unscoped path".to_string(),
        RouteTarget::Identity => "/user".to_string(),
    }
}

/// Step 2 of the write pipeline: the call's target is the permit's (the same slug, or both
/// Identity), and its origin one the target may be written on (a repository's: the api or
/// the uploads origin; the identity's: the api origin). Usage otherwise, before anything is
/// loaded or sent.
pub(crate) fn write_target(call: &Call, permit: &Permit) -> Result<()> {
    let (same, origin_ok) = match (&call.route.target, permit.target()) {
        (RouteTarget::Repo(a), PermitTarget::Repo(b)) => (
            a == b,
            matches!(call.origin, OriginRole::Api | OriginRole::Uploads),
        ),
        (RouteTarget::Identity, PermitTarget::Identity) => (true, call.origin == OriginRole::Api),
        _ => (false, true),
    };
    let permitted = match permit.target() {
        PermitTarget::Repo(slug) => slug.to_string(),
        PermitTarget::Identity => "/user".to_string(),
    };
    if !same {
        return Err(Error::usage(format!(
            "the call is for {}, the permit for {permitted}",
            route_target_text(&call.route.target)
        )));
    }
    if !origin_ok {
        return Err(Error::usage(format!(
            "a write to {permitted} goes to the API origin{}, not the {} origin",
            if matches!(permit.target(), PermitTarget::Repo(_)) {
                " or the uploads origin"
            } else {
                ""
            },
            origin_word(call.origin)
        )));
    }
    Ok(())
}

/// `error` naming the call's repository, when it names none yet.
fn with_call_repo(call: &Call, error: Error) -> Error {
    match repo_of(&call.route) {
        Some(slug) if error.repo.is_none() => error.with_repo(slug.to_string()),
        _ => error,
    }
}

/// A login binding's error as the write's: the write itself was never sent, so the error
/// carries no HTTP exchange and no side effect (the binding's `GET /user` status moves into
/// its why), and `write::settle_table` settles it `NeverSent` rather than reading the read's
/// 5xx as an unknown write or its 4xx as a refused one.
fn before_the_write(mut error: Error) -> Error {
    if let Some(http) = error.http.take() {
        error.why = format!(
            "the login binding's GET /user answered {}, and the write was not sent: {}",
            http.status, error.why
        );
    }
    error.side_effect = SideEffect::None;
    error
}

/// `api`, `uploads`, `web`, `CDN`.
const fn origin_word(role: OriginRole) -> &'static str {
    match role {
        OriginRole::Api => "API",
        OriginRole::Uploads => "uploads",
        OriginRole::Web => "web",
        OriginRole::AssetCdn => "CDN",
    }
}

/// Unauthenticated: the profile's credential did not load.
fn not_loaded(profile: &str) -> Error {
    Error::new(
        Kind::Unauthenticated,
        format!("the credential of {profile} did not load"),
    )
}

/// CouldNotRun `Sub::Local` about the upload file at `path`.
fn file_error(path: &std::path::Path, what: &str) -> Error {
    Error::local(format!("{} {what}", path.display()))
}

/// How a Converging write's guard is asked to look before an attempt (`Github::converging`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Look {
    /// The attempt about to be sent (from 1).
    pub(crate) attempt: u32,
    /// Whether an earlier attempt of this call lost its answer (`side_effect` unknown).
    pub(crate) lost: bool,
    /// The read after a lost answer the loop will not retry: nothing is sent after it, so
    /// the guard only says whether the state is already right. `Github::converging` never asks
    /// it; `Releases::converge` does, after the loop.
    pub(crate) last: bool,
}

/// What ended a Converging write's attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    /// An attempt was answered with a 2xx, or the guard found the state right.
    Answered,
    /// The guard's own error.
    Guard,
    /// An attempt's error the policy does not retry.
    Attempt,
    /// The pause before a retry failed.
    Pause,
}

/// What a Converging write's attempts came to (`Github::converging`).
pub(crate) struct ConvergingAttempts {
    /// The answer, or the error that ended the loop (`ended` says whose).
    pub(crate) result: Result<Answered>,
    /// What ended the loop.
    pub(crate) ended: Ended,
    /// The error of the latest attempt that lost its answer (side effect unknown), kept to
    /// the end.
    pub(crate) lost: Option<Error>,
    /// The failure of the last attempt the loop retried past: the one before the attempt that
    /// ended it, or, for `Ended::Pause`, the one the failed pause followed.
    pub(crate) previous: Option<Error>,
    /// Whether an attempt the loop retried past may have landed (its side effect unknown).
    pub(crate) earlier: bool,
    /// The attempt the loop ended at.
    pub(crate) attempt: u32,
}

/// `Github::converging`'s state between attempts.
struct Progress {
    lost: Option<Error>,
    previous: Option<Error>,
    earlier: bool,
    attempt: u32,
}

impl Progress {
    /// The attempts, ended by `ended` with `result`.
    fn end(self, result: Result<Answered>, ended: Ended) -> ConvergingAttempts {
        ConvergingAttempts {
            result,
            ended,
            lost: self.lost,
            previous: self.previous,
            earlier: self.earlier,
            attempt: self.attempt,
        }
    }
}

/// The stronger of two side effects: `Applied` over `Unknown` over `None`.
const fn stronger(a: SideEffect, b: SideEffect) -> SideEffect {
    match (a, b) {
        (SideEffect::Applied, _) | (_, SideEffect::Applied) => SideEffect::Applied,
        (SideEffect::Unknown, _) | (_, SideEffect::Unknown) => SideEffect::Unknown,
        (SideEffect::None, SideEffect::None) => SideEffect::None,
    }
}

/// `error`, saying at least what an earlier attempt of the same write may have done.
fn at_least(error: Error, earlier: SideEffect) -> Error {
    let side_effect = stronger(error.side_effect, earlier);
    if side_effect == error.side_effect {
        error
    } else {
        error.with_side_effect(side_effect)
    }
}

/// The file at `path`, opened again for this attempt, when it still holds `len` bytes: a
/// file that changed is could_not_run before any byte leaves. The open never waits (a FIFO put in
/// the file's place is refused on the handle, not waited on).
fn open_unchanged(path: &std::path::Path, len: u64) -> Result<std::fs::File> {
    let file = crate::download::open_nonblocking(path)
        .map_err(|e| file_error(path, &format!("could not be opened: {e}")))?;
    let meta = file
        .metadata()
        .map_err(|e| file_error(path, &format!("could not be read: {e}")))?;
    if !meta.is_file() || meta.len() != len {
        return Err(file_error(path, "changed since it was hashed"));
    }
    Ok(file)
}

/// `sha256:{hex}` of `file`'s bytes, which must be `len` of them; the file is left at its start.
fn file_digest(file: &mut std::fs::File, path: &std::path::Path, len: u64) -> Result<String> {
    use std::io::{Seek, SeekFrom};
    let mut hasher = aterm_digest::Sha256::new();
    let mut buf = [0u8; 16 * 1024];
    let mut read = 0u64;
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
                read = read.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(file_error(path, &format!("could not be read: {e}"))),
        }
    }
    if read != len {
        return Err(file_error(path, "changed since it was hashed"));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| file_error(path, &format!("could not be read: {e}")))?;
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    aterm_codec::hex::push_lower(&mut out, &hasher.finalize());
    Ok(out)
}

/// The call's body as the wire takes it, and a file body's digest when `digest` asks for it
/// (a tape names a file body by its digest). A file is opened into `file`, which the body
/// borrows.
fn write_body<'a>(
    call: &'a Call,
    file: &'a mut Option<std::fs::File>,
    digest: bool,
) -> Result<(WireBody<'a>, Option<String>)> {
    match &call.body {
        CallBody::Empty => Ok((WireBody::Empty, None)),
        CallBody::Json(bytes) => Ok((WireBody::Bytes(bytes), None)),
        CallBody::File { path, len, .. } => {
            let opened = file.insert(open_unchanged(path, *len)?);
            let sha = if digest {
                Some(file_digest(opened, path, *len)?)
            } else {
                None
            };
            Ok((
                WireBody::Reader {
                    len: *len,
                    reader: opened,
                },
                sha,
            ))
        }
    }
}

fn method_gate(call: &Call) -> Result<()> {
    if call.method.is_read() {
        Ok(())
    } else {
        Err(method_refusal(call.method, repo_of(&call.route)))
    }
}

fn head_of(response: WireResponse) -> (Vec<(String, String)>, Vec<u8>) {
    let WireResponse { headers, body, .. } = response;
    let mut head = Vec::new();
    body.take(HEAD_MAX)
        .read_to_end(&mut head)
        .unwrap_or_default();
    (headers, head)
}

fn moved_id(location: &str, endpoints: &Endpoints) -> Option<u64> {
    let (origin, path_and_query) = Origin::parse(location)?;
    if &origin != endpoints.api() {
        return None;
    }
    let rest = path_and_query.strip_prefix("/repositories/")?;
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    crate::path::digits(&rest[..end])
}

impl Github {
    fn credential(&self, profile: &str) -> Option<Arc<Credential>> {
        match self
            .inner
            .credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(profile)
        {
            Some(Ok(cred)) => Some(cred.clone()),
            _ => None,
        }
    }

    /// Read step 1: the decision the route door gives (live, `decide`'s own; under a
    /// credentialed replay, the tape's), then the credential it sends. The first send as an
    /// inline profile binds its login first.
    fn route(&self, route: &RouteRequest) -> Result<Decided> {
        let (decided, profile) = self.route_unbound(route)?;
        if let Some(profile) = profile {
            self.bind_login(&profile)?;
        }
        Ok(decided)
    }

    /// `route` with no login binding: the decision and the profile it sends as, if any. The
    /// binding's own `GET /user` routes here, so it never waits on itself.
    fn route_unbound(&self, route: &RouteRequest) -> Result<(Decided, Option<String>)> {
        match self.inner.fx.route(route, &mut || self.decide(route))? {
            Decision::Send { profile, .. } => {
                let decided = self.decided_as(&profile)?;
                Ok((decided, Some(profile)))
            }
            Decision::Anonymous { why } => {
                if why != AnonymousWhy::Asked {
                    self.inner.meter.lock_or_recover().fell_to_anonymous = true;
                }
                Ok((
                    Decided {
                        identity: IdentityRef::Anonymous,
                        cred: None,
                        covers_all_repos: false,
                        taped: false,
                    },
                    None,
                ))
            }
            Decision::Refuse(refusal) => Err(refusal.into_error(repo_of(route))),
        }
    }

    /// Whether this run replays a tape: then no credential is loaded or sent.
    fn replays(&self) -> bool {
        self.inner.fx.mode().replays()
    }

    /// Whether this run is under any tape.
    fn taped(&self) -> bool {
        self.inner.fx.mode().is_taped()
    }

    /// What a Send as `profile` speaks with: its loaded credential, or, under a replay, no
    /// credential and the fingerprint `taped`.
    fn decided_as(&self, profile: &str) -> Result<Decided> {
        // A replay's router is empty, so the flag is a routing fact: a recording reads it off
        // the live router as the untaped run does and tapes it, and a replay reads it back.
        let covers = self.inner.fx.fact(COVERS, profile, &mut || {
            let covers = self
                .inner
                .router
                .profile(profile)
                .is_some_and(|p| p.covers_all_repos);
            covers.to_string()
        })?;
        let covers_all_repos = covers == "true";
        if self.replays() {
            return Ok(Decided {
                identity: IdentityRef::Profile {
                    name: profile.to_string(),
                    fingerprint: TAPED.to_string(),
                },
                cred: None,
                covers_all_repos,
                taped: true,
            });
        }
        let cred = self
            .credential(profile)
            .ok_or_else(|| not_loaded(profile))?;
        Ok(Decided {
            identity: IdentityRef::Profile {
                name: profile.to_string(),
                fingerprint: cred.fingerprint().to_string(),
            },
            cred: Some(cred),
            covers_all_repos,
            taped: false,
        })
    }

    /// What a write under `permit` speaks with: `decided_as` the permit's profile, its
    /// credential loaded (once per process) first.
    fn decided_for(&self, permit: &Permit) -> Result<Decided> {
        if !self.replays() {
            let _ = self.presence(permit.profile());
        }
        self.decided_as(permit.profile())
    }

    /// Login binding (design §6.1): before the first send as an inline profile in this
    /// client, one `GET /user` as that profile, whose `login` must be the profile's
    /// (ASCII case-insensitive). Unauthenticated `Sub::LoginMismatch` otherwise, and nothing
    /// else is sent. A profile of a file or the environment needs no binding.
    ///
    /// A send that finds another send of this client binding the same profile waits for that
    /// binding to end, so nothing else goes out as the profile while its login is unchecked.
    /// A binding that did not bind wakes its waiters, and each binds again itself.
    fn bind_login(&self, profile: &str) -> Result<()> {
        let Some(row) = self.inner.router.profile(profile) else {
            return Ok(());
        };
        if !matches!(row.source, Source::Inline(_)) {
            return Ok(());
        }
        let mut held = self.inner.bound.lock_or_recover();
        loop {
            match held.profiles.get(profile) {
                Some(true) => return Ok(()),
                Some(false) => {
                    #[cfg(any(test, debug_assertions))]
                    {
                        held.waiting = held.waiting.saturating_add(1);
                    }
                    held = self
                        .inner
                        .bind_done
                        .wait(held)
                        .unwrap_or_else(PoisonError::into_inner);
                    #[cfg(any(test, debug_assertions))]
                    {
                        held.waiting = held.waiting.saturating_sub(1);
                    }
                }
                None => break,
            }
        }
        held.profiles.insert(profile.to_string(), false);
        drop(held);
        let mut binder = Binder {
            gh: self,
            profile,
            bound: false,
        };
        let expected = row.login.clone().unwrap_or_default();
        let route = RouteRequest::read(RouteTarget::Identity).with_as(Some(profile.to_string()));
        let call = Call::get("/user", route)
            .probe_404(false)
            .follow_moved(false);
        // Routed past the binding: this read is the binding's own send as the profile.
        let got = self
            .route_unbound(&call.route)
            .and_then(|(decided, _)| self.run(&call, &decided, Reading::Whole))
            .and_then(Got::into_answered)
            .and_then(|answered| {
                let user: crate::model::UserObject = crate::model::from_json(&answered.json()?)?;
                Ok(user.login)
            })?;
        if got.eq_ignore_ascii_case(&expected) {
            binder.bound = true;
            return Ok(());
        }
        Err(Error::new(
            Kind::Unauthenticated,
            format!("the token of {profile} is {got}'s, and {profile} names {expected}"),
        )
        .with_sub(Sub::LoginMismatch { expected, got }))
    }

    /// Pacing (design §7.4): a write attempt begins at least 1 s of `fx.mono()` after this
    /// client's last one began, sleeping through `fx.sleep` for the rest. No lock spans
    /// processes.
    fn pace(&self) -> Result<()> {
        let fx = &self.inner.fx;
        let mut last = self
            .inner
            .last_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(began) = *last {
            let elapsed = fx.mono()?.saturating_sub(began);
            if elapsed < PACE {
                fx.sleep(PACE.saturating_sub(elapsed))?;
            }
        }
        *last = Some(fx.mono()?);
        Ok(())
    }

    /// `prepare` for a write: its body's `Content-Type` added.
    fn prepare_write(&self, call: &Call, decided: &Decided) -> Prepared {
        let mut prepared = self.prepare(call, decided, Reading::Whole);
        let content_type = match &call.body {
            CallBody::Empty => None,
            CallBody::Json(_) => Some("application/json"),
            CallBody::File { content_type, .. } => Some(content_type.as_str()),
        };
        if let Some(content_type) = content_type {
            prepared
                .headers
                .push(("Content-Type".to_string(), content_type.to_string()));
        }
        prepared
    }

    /// One write attempt: the rate book's pre-refusal (unless `consult_book` is off), the
    /// body opened again, one exchange through `Fx::gh`, and the answer classified. Every
    /// error says what the write may have done: `side_effect` none when nothing was sent or
    /// GitHub answered a 4xx, unknown for a 5xx, a redirect, or a request that may have
    /// left with no answer read.
    fn write_attempt(
        &self,
        call: &Call,
        permit: &Permit,
        decided: &Decided,
        consult_book: bool,
    ) -> Result<Answered> {
        let prepared = self.prepare_write(call, decided);
        self.check_anonymous_api(&prepared)?;
        let key = book_key_str(&decided.identity);
        let booked = call.origin == OriginRole::Api && self.inner.rate_book.enabled();
        if booked && consult_book {
            let now = self.inner.fx.wall_secs()?;
            let resource = resource_for(&call.path);
            if let Some(refused) = self.inner.rate_book.refuse_before_send(key, resource, now) {
                return Err(self.finish_error(refused, call, &prepared.as_, None));
            }
        }
        let mut file = None;
        let (body, digest) = write_body(call, &mut file, self.taped())?;
        let request = prepared.write_request(permit, body, digest.as_deref());
        {
            let mut meter = self.inner.meter.lock_or_recover();
            meter.requests = meter.requests.saturating_add(1);
            meter.as_ = Some(prepared.as_.clone());
            if matches!(prepared.origin, OriginRole::Api | OriginRole::Uploads) {
                meter.note_rest_request(&prepared.as_);
            }
        }
        let exchanged = self
            .inner
            .fx
            .gh(&*self.inner.transport, &self.inner.endpoints, request);
        let answer = match exchanged {
            Ok(answer) => answer,
            Err(door) => {
                // A recording sent the write before the tape refused its answer. A latch an
                // earlier exchange set never reaches here: `pace` reads `fx.mono()` just
                // before this attempt, and a recording's clock returns the latched error, so
                // the run ends there, sent nothing, at side_effect none.
                let after = self.inner.fx.mode() == Mode::Recording
                    && door.sub == Sub::Replay(Replay::BodyNotTaped);
                let door = if after {
                    door.with_side_effect(SideEffect::Unknown)
                } else {
                    door
                };
                return Err(self.finish_error(door, call, &prepared.as_, None));
            }
        };
        self.record_exchange(call, &prepared.as_, &answer);
        let mut response = match answer {
            Ok(response) => response,
            Err(wire) => {
                let mut error = classify::wire(&wire, call.method, call.idempotency);
                if wire.phase != crate::wire::Phase::NotSent {
                    error = error.with_side_effect(SideEffect::Unknown);
                }
                return Err(self.finish_error(error, call, &prepared.as_, None));
            }
        };
        let rate = RateSnapshot::from_headers(&response.headers);
        if let Some(snapshot) = &rate {
            self.inner.meter.lock_or_recover().rate = Some(snapshot.clone());
            if booked {
                self.inner.rate_book.record(key, snapshot);
            }
        }
        let status = response.status;
        if (200..300).contains(&status) {
            return match prepared.held(&mut response) {
                Ok(whole) => {
                    Got::of(response, whole, &prepared.as_, 1, rate, call.limit).into_answered()
                }
                Err(wire) => {
                    let error = classify::wire(&wire, call.method, call.idempotency)
                        .with_side_effect(SideEffect::Unknown);
                    let exchange = Exchange {
                        status,
                        headers: &response.headers,
                        rate,
                    };
                    Err(self.finish_error(error, call, &prepared.as_, Some(exchange)))
                }
            };
        }
        let (headers, head) = head_of(response);
        let side_effect = if (400..500).contains(&status) {
            SideEffect::None
        } else {
            SideEffect::Unknown
        };
        let error = self
            .classify_answer(status, &headers, &head, call.method, call.idempotency)?
            .unwrap_or_else(|| Error::protocol(format!("GitHub answered {status}")))
            .with_side_effect(side_effect);
        let exchange = Exchange {
            status,
            headers: &headers,
            rate,
        };
        Err(self.finish_error(error, call, &prepared.as_, Some(exchange)))
    }

    fn prepare(&self, call: &Call, decided: &Decided, reading: Reading) -> Prepared {
        let endpoints = &self.inner.endpoints;
        let mut headers = Vec::with_capacity(5);
        headers.push(("Accept".to_string(), call.accept.header_value().to_string()));
        headers.push(("User-Agent".to_string(), self.inner.user_agent.clone()));
        if matches!(call.origin, OriginRole::Api | OriginRole::Uploads) {
            headers.push((
                "X-GitHub-Api-Version".to_string(),
                crate::API_VERSION.to_string(),
            ));
        }
        if let Some(etag) = &call.if_none_match {
            headers.push(("If-None-Match".to_string(), etag.clone()));
        }
        let origin = endpoints.origin(call.origin);
        let auth = decided
            .cred
            .as_deref()
            .and_then(|cred| AuthHeader::for_origin(cred, endpoints, origin));
        let deadlines = match self.inner.backstop {
            Some(backstop) => call.deadlines.with_backstop(backstop),
            None => call.deadlines,
        };
        // A replay loads no credential, so it names the identity wherever a live run's
        // credential would have gone: the api and uploads origins.
        let credential_origin = origin == endpoints.api() || origin == endpoints.uploads();
        let as_ = if auth.is_some() || (decided.taped && credential_origin) {
            decided.identity.clone()
        } else {
            IdentityRef::Anonymous
        };
        let as_profile = match &decided.identity {
            IdentityRef::Profile { name, .. } => Some(name.clone()),
            IdentityRef::Anonymous => None,
        };
        Prepared {
            origin: call.origin,
            atomic_refs: call.atomic_refs.clone(),
            repo_create: call.repo_create.clone(),
            method: call.method,
            url: endpoints.url(call.origin, &call.path_and_query_cow()),
            headers,
            auth,
            as_,
            as_profile,
            limit: call.limit,
            reading,
            deadlines,
        }
    }

    fn check_anonymous_api(&self, prepared: &Prepared) -> Result<()> {
        if self.inner.no_anonymous_api
            && matches!(prepared.origin, OriginRole::Api | OriginRole::Uploads)
            && prepared.as_ == IdentityRef::Anonymous
        {
            return Err(Error::refused(
                "anonymous API budget is zero; select an available authenticated reader or a quota-free public read",
            ));
        }
        Ok(())
    }

    fn send(&self, prepared: &Prepared) -> Result<std::result::Result<WireResponse, WireError>> {
        self.check_anonymous_api(prepared)?;
        {
            let mut meter = self.inner.meter.lock_or_recover();
            meter.requests = meter.requests.saturating_add(1);
            meter.as_ = Some(prepared.as_.clone());
            if matches!(prepared.origin, OriginRole::Api | OriginRole::Uploads) {
                meter.note_rest_request(&prepared.as_);
            }
        }
        let transport = &*self.inner.transport;
        match prepared.reading {
            Reading::Whole => {
                self.inner
                    .fx
                    .gh(transport, &self.inner.endpoints, prepared.request())
            }
            Reading::Stream => self.inner.fx.gh_stream(transport, prepared.request()),
        }
    }

    fn finish_error(
        &self,
        error: Error,
        call: &Call,
        as_: &IdentityRef,
        exchange: Option<Exchange<'_>>,
    ) -> Error {
        let mut error = error.with_identity(as_.clone());
        if let Some(exchange) = exchange {
            error = error.with_http(HttpRef {
                status: exchange.status,
                method: call.method.as_str(),
                path: path_only(&call.path).to_string(),
                request_id: header_in(exchange.headers, "x-github-request-id").map(str::to_string),
            });
            if let Some(rate) = exchange.rate {
                error = error.with_rate(rate);
            }
        }
        if error.repo.is_none()
            && let Some(slug) = repo_of(&call.route)
        {
            error = error.with_repo(slug.to_string());
        }
        error
    }

    /// Classify one answer with a clock only when `Retry-After` is not delta-seconds. This keeps
    /// ordinary exchanges off the clock door while admitting a valid HTTP-date deterministically.
    fn classify_answer(
        &self,
        status: u16,
        headers: &[(String, String)],
        body: &[u8],
        method: Method,
        idempotency: Idempotency,
    ) -> Result<Option<Error>> {
        let now = match (status, header_in(headers, "retry-after").map(str::trim)) {
            (403 | 429, Some(value))
                if aterm_types::http_retry::retry_after_delay_secs(value, None).is_none() =>
            {
                Some(self.inner.fx.wall_secs()?)
            }
            _ => None,
        };
        Ok(classify::answer_at(
            status,
            headers,
            body,
            method,
            idempotency,
            now,
        ))
    }

    fn run(&self, call: &Call, decided: &Decided, reading: Reading) -> Result<Got> {
        let mut sent = 0;
        self.run_counted(call, decided, reading, &mut sent)
    }

    /// Retain each API attempt, including retries, transport failures and responses
    /// without quota headers. Attribution failure leaves write outcomes intact.
    fn record_exchange(
        &self,
        call: &Call,
        identity: &IdentityRef,
        exchanged: &std::result::Result<WireResponse, WireError>,
    ) {
        if self.replays()
            || !self.inner.rate_book.enabled()
            || !matches!(call.origin, OriginRole::Api | OriginRole::Uploads)
        {
            return;
        }
        let record = || {
            let now = self.inner.fx.wall_secs()?;
            let response = exchanged.as_ref().ok();
            let rate = response.and_then(|response| RateSnapshot::from_headers(&response.headers));
            let program = std::env::current_exe().ok().and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            });
            let row = aterm_json::json!({
                "v": 1,
                "at": now,
                "host": aterm_uds::process::kernel_host_name(),
                "pid": std::process::id(),
                "parent_pid": i32::try_from(std::process::id()).ok()
                    .and_then(aterm_uds::process::read_process_parent),
                "program": program,
                "caller": self.redactor().apply(&self.inner.caller),
                "as": identity.label(),
                "fingerprint": book_key_str(identity),
                "method": call.method.as_str(),
                "path": self.redactor().apply(path_only(&call.path)),
                "status": response.map(|response| response.status),
                "wire_error": exchanged.as_ref().err().map(|error| error.kind.slug()),
                "wire_phase": exchanged.as_ref().err().map(|error| match error.phase {
                    crate::wire::Phase::NotSent => "not_sent",
                    crate::wire::Phase::Sent => "sent",
                    crate::wire::Phase::Answered => "answered",
                }),
                "rate": rate.as_ref().map(RateSnapshot::to_json),
            });
            self.inner.rate_book.record_call(now, &row)
        };
        if let Err(error) = record() {
            let _ = self.inner.fx.note_rate(&self.redactor().apply(&error.why));
        }
    }

    fn run_counted(
        &self,
        call: &Call,
        decided: &Decided,
        reading: Reading,
        sent: &mut u32,
    ) -> Result<Got> {
        self.run_counted_with(call, decided, reading, sent, None, &mut Ok)
    }

    fn run_counted_with<T>(
        &self,
        call: &Call,
        decided: &Decided,
        reading: Reading,
        sent: &mut u32,
        mut resume: Option<&mut ProgressResume<'_>>,
        consume: &mut impl FnMut(Got) -> Result<T>,
    ) -> Result<T> {
        let mut prepared = self.prepare(call, decided, reading);
        let key = book_key_str(&decided.identity);
        let booked = call.origin == OriginRole::Api && self.inner.rate_book.enabled();
        let free = path_only(&call.path) == "/rate_limit";
        if booked && !free && call.rate_book {
            let now = self.inner.fx.wall_secs()?;
            let resource = resource_for(&call.path);
            if let Some(refused) = self.inner.rate_book.refuse_before_send(key, resource, now) {
                return Err(self.finish_error(refused, call, &prepared.as_, None));
            }
        }
        let configured = call.retry.unwrap_or(self.inner.retry);
        // A streamed hop is one composite read whose second request is always rebuilt without
        // credentials. Select its anonymous budget before the first request so API and CDN
        // failures consume one bound rather than giving the CDN the credentialed 3-attempt
        // policy (or opening a nested budget after the redirect).
        let mut policy = if prepared.as_ == IdentityRef::Anonymous
            || (call.hop && reading == Reading::Stream && call.method.is_read())
        {
            configured.for_anonymous_http()
        } else {
            configured
        };
        let mut requests = 0u32;
        let mut attempt = 1u32;
        let mut retry_budget = policy.budget();
        let mut slept = Duration::ZERO;
        loop {
            let mut anonymous_subread = false;
            let began = resume
                .as_mut()
                .map_or(0, |resume| resume.begin(&mut prepared));
            requests = requests.saturating_add(1);
            *sent = requests;
            let exchanged = self.send(&prepared)?;
            self.record_exchange(call, &prepared.as_, &exchanged);
            let failure = match exchanged {
                Err(wire) => self.finish_error(
                    classify::wire(&wire, call.method, call.idempotency),
                    call,
                    &prepared.as_,
                    None,
                ),
                Ok(mut response) => {
                    let rate = RateSnapshot::from_headers(&response.headers);
                    if let Some(snapshot) = &rate {
                        self.inner.meter.lock_or_recover().rate = Some(snapshot.clone());
                        if booked {
                            self.inner.rate_book.record(key, snapshot);
                        }
                    }
                    let retry_answer = if call.answer_any {
                        crate::retry::transient_cause(
                            &self
                                .classify_answer(
                                    response.status,
                                    &response.headers,
                                    &[],
                                    call.method,
                                    call.idempotency,
                                )?
                                .unwrap_or_else(|| {
                                    Error::protocol(format!("GitHub answered {}", response.status))
                                })
                                .with_http(HttpRef {
                                    status: response.status,
                                    method: call.method.as_str(),
                                    path: path_only(&call.path).to_string(),
                                    request_id: None,
                                }),
                        )
                        .is_some()
                    } else {
                        false
                    };
                    if (call.answer_any && !retry_answer) || is_success(response.status, reading) {
                        let stream_limit = prepared.stream_limit(response.status);
                        match prepared.held(&mut response) {
                            Ok(whole) => {
                                let got = Got::of(
                                    response,
                                    whole,
                                    &prepared.as_,
                                    requests,
                                    rate,
                                    stream_limit,
                                );
                                match consume(got) {
                                    Ok(value) => return Ok(value),
                                    Err(error) => {
                                        self.finish_error(error, call, &prepared.as_, None)
                                    }
                                }
                            }
                            Err(wire) => {
                                let exchange = Exchange {
                                    status: response.status,
                                    headers: &response.headers,
                                    rate,
                                };
                                self.finish_error(
                                    classify::wire(&wire, call.method, call.idempotency),
                                    call,
                                    &prepared.as_,
                                    Some(exchange),
                                )
                            }
                        }
                    } else {
                        let status = response.status;
                        let (headers, head) = head_of(response);
                        let classified = self
                            .classify_answer(
                                status,
                                &headers,
                                &head,
                                call.method,
                                call.idempotency,
                            )?
                            .unwrap_or_else(|| {
                                Error::protocol(format!("GitHub answered {status}"))
                            });
                        let exchange = Exchange {
                            status,
                            headers: &headers,
                            rate,
                        };
                        let error =
                            self.finish_error(classified, call, &prepared.as_, Some(exchange));
                        let failed = Failed {
                            status,
                            headers: &headers,
                            error,
                            requests,
                        };
                        match self.judge(call, decided, &prepared, failed)? {
                            Flow::Done(got) => match consume(*got) {
                                Ok(value) => return Ok(value),
                                Err(error) => self.finish_error(error, call, &prepared.as_, None),
                            },
                            Flow::Retry {
                                error,
                                beyond,
                                anonymous,
                            } => {
                                anonymous_subread = anonymous;
                                requests = requests.saturating_add(beyond);
                                *sent = requests;
                                error
                            }
                        }
                    }
                }
            };
            let anonymous_policy = configured.for_anonymous_http();
            if anonymous_subread && policy != anonymous_policy {
                policy = anonymous_policy;
                let Some(widened) = policy.budget_with_paused(slept) else {
                    return Err(retry_exhausted(failure, &policy.bound()));
                };
                retry_budget = widened;
            }
            if let Some(resume) = resume.as_mut()
                && let Some(cause) = transient_cause(&failure)
            {
                let (moved, after) = resume.after(began);
                if moved != 0 {
                    if !resume.admit(moved, after) {
                        return Err(retry_exhausted(failure, &resume.bound()));
                    }
                    self.inner.fx.note_retry(resume.logged_retry(cause))?;
                    attempt = 1;
                    retry_budget = policy.budget();
                    continue;
                }
            }
            let retry = match should_retry(&mut retry_budget, call.idempotency, attempt, &failure) {
                RetryDecision::Retry(retry) => retry,
                RetryDecision::Exhausted { bound } => {
                    return Err(retry_exhausted(failure, &bound));
                }
                RetryDecision::Final => return Err(failure),
            };
            let pause = retry.pause;
            if let Some(budget) = call.sleep_budget
                && slept.saturating_add(pause) > budget
            {
                let bound = format!("{}s call retry-pause bound", budget.as_secs());
                return Err(retry_exhausted(failure, &bound));
            }
            self.inner.fx.note_retry(retry)?;
            self.inner.fx.sleep(pause)?;
            slept = slept.saturating_add(pause);
            attempt = attempt.saturating_add(1);
        }
    }
}

struct Failed<'a> {
    status: u16,
    headers: &'a [(String, String)],
    error: Error,
    /// Requests sent for this call so far.
    requests: u32,
}

impl Github {
    fn judge(
        &self,
        call: &Call,
        decided: &Decided,
        prepared: &Prepared,
        failed: Failed<'_>,
    ) -> Result<Flow> {
        let reading = prepared.reading;
        let location = header_in(failed.headers, "location");
        match failed.status {
            301 => {
                let id = location.and_then(|l| moved_id(l, &self.inner.endpoints));
                match id {
                    Some(id)
                        if call.method.is_read()
                            && call.route.access == Access::Read
                            && call.origin == OriginRole::Api
                            && call.follow_moved =>
                    {
                        self.follow_move(call, id, failed, reading)
                    }
                    _ => Err(failed.error),
                }
            }
            302 | 307 if call.hop && reading == Reading::Stream => {
                self.hop(call, prepared, location, failed)
            }
            status @ 300..=399 if call.method.is_read() => Err(protocol_error(
                failed.error,
                format!("GitHub answered {status} and agh follows no redirect here"),
            )),
            300..=399 => Err(failed.error),
            404 if call.origin == OriginRole::Api => match repo_of(&call.route) {
                Some(slug) => self.judge_404(call, decided, prepared, slug, failed),
                None => Ok(Flow::Retry {
                    error: failed.error,
                    beyond: 0,
                    anonymous: false,
                }),
            },
            _ => Ok(Flow::Retry {
                error: failed.error,
                beyond: 0,
                anonymous: false,
            }),
        }
    }

    fn follow_move(
        &self,
        call: &Call,
        id: u64,
        failed: Failed<'_>,
        reading: Reading,
    ) -> Result<Flow> {
        let Some(old) = repo_of(&call.route).cloned() else {
            return Err(failed.error);
        };
        if after_prefix(&call.path, &repo_root(&old)).is_none() {
            return Err(failed.error);
        }
        let lookup = Call::get(
            format!("/repositories/{id}"),
            RouteRequest::read(RouteTarget::Unscoped).with_anonymous(true),
        )
        .follow_moved(false)
        .probe_404(false)
        // The lookup is one subrequest of this logical read. Its transient failure returns to
        // the outer call's one retry budget below rather than opening a nested budget.
        .retry(Some(RetryPolicy::NONE))
        .deadlines(call.deadlines);
        let lookup_decided = self.route(&lookup.route)?;
        let mut lookup_sent = 0;
        let looked = match self
            .run_counted(&lookup, &lookup_decided, Reading::Whole, &mut lookup_sent)
            .and_then(Got::into_answered)
        {
            Ok(looked) => looked,
            Err(error) if error.kind == Kind::NotFound => {
                return Err(error
                    .recast(
                        Kind::NotVisible,
                        "the repository moved, and its new name is not visible anonymously",
                    )
                    .with_sub(Sub::Moved {
                        to: format!("repositories/{id}"),
                    })
                    .with_next("name its new owner/name with -R")
                    .with_repo(old.to_string()));
            }
            Err(error) => {
                let error = if error.repo.is_none() {
                    error.with_repo(old.to_string())
                } else {
                    error
                };
                if transient_cause(&error).is_some() {
                    return Ok(Flow::Retry {
                        error,
                        beyond: lookup_sent,
                        anonymous: true,
                    });
                }
                return Err(error);
            }
        };
        let full_name = looked
            .json()
            .map_err(|error| error.with_repo(old.to_string()))?
            .get("full_name")
            .and_then(aterm_json::Value::as_str)
            .map(str::to_string);
        let new = full_name
            .as_deref()
            .and_then(|name| Slug::parse(name).ok())
            .ok_or_else(|| {
                Error::protocol(format!(
                    "GitHub's answer for repositories/{id} names no repository"
                ))
                .with_repo(old.to_string())
            })?;
        self.remember_repo_id(&new, id);
        let mut moved = call.clone();
        retarget(&mut moved, &old, &new);
        moved.follow_moved = false;
        moved.retry = Some(RetryPolicy::NONE);
        let decided = self.route(&moved.route)?;
        let mut moved_sent = 0;
        let mut got = match self.run_counted(&moved, &decided, reading, &mut moved_sent) {
            Ok(got) => got,
            Err(error) if transient_cause(&error).is_some() => {
                return Ok(Flow::Retry {
                    error,
                    beyond: lookup_sent.saturating_add(moved_sent),
                    anonymous: true,
                });
            }
            Err(error) => return Err(error),
        };
        got.requests = got
            .requests
            .saturating_add(failed.requests)
            .saturating_add(lookup_sent);
        got.moved = Some((old, new));
        self.inner
            .meter
            .lock_or_recover()
            .moved
            .clone_from(&got.moved);
        Ok(Flow::Done(Box::new(got)))
    }

    fn hop(
        &self,
        call: &Call,
        prepared: &Prepared,
        location: Option<&str>,
        failed: Failed<'_>,
    ) -> Result<Flow> {
        let Some(location) = location else {
            let why = format!("GitHub answered {} with no Location", failed.status);
            return Err(protocol_error(failed.error, why));
        };
        let rebuilt = {
            let original = prepared.request();
            hop::rebuild(
                &original,
                location,
                &self.inner.endpoints,
                &self.inner.user_agent,
            )
        };
        let request = match rebuilt {
            Ok(request) => request,
            Err(refused) => {
                let refused = refused.into_data();
                return Err(failed
                    .error
                    .recast(refused.kind, refused.why)
                    .with_sub(refused.sub)
                    .with_next(refused.next));
            }
        };
        {
            let mut meter = self.inner.meter.lock_or_recover();
            meter.requests = meter.requests.saturating_add(1);
            meter.hopped = true;
        }
        let requests = failed.requests.saturating_add(1);
        let redirected = Tried::Sent {
            as_: prepared.as_.label().to_string(),
            status: failed.status,
        };
        let response = match self.inner.fx.gh_stream(&*self.inner.transport, request)? {
            Ok(response) => response,
            Err(wire) => {
                let error = classify::wire(&wire, call.method, call.idempotency)
                    .with_tried(vec![redirected]);
                let error = self.finish_error(error, call, &IdentityRef::Anonymous, None);
                return Ok(Flow::Retry {
                    error,
                    beyond: 1,
                    anonymous: true,
                });
            }
        };
        let status = response.status;
        if is_success(status, Reading::Stream) {
            let stream_limit = prepared.stream_limit(status);
            let mut got = Got::of(response, None, &prepared.as_, requests, None, stream_limit);
            got.hopped = true;
            return Ok(Flow::Done(Box::new(got)));
        }
        let (headers, head) = head_of(response);
        let error = if (300..400).contains(&status) {
            Error::protocol("a second redirect")
        } else {
            self.classify_answer(status, &headers, &head, call.method, call.idempotency)?
                .unwrap_or_else(|| Error::protocol(format!("the hop answered {status}")))
        };
        let error = error.with_tried(vec![
            redirected,
            Tried::Sent {
                as_: IdentityRef::Anonymous.label().to_string(),
                status,
            },
        ]);
        let exchange = Exchange {
            status,
            headers: &headers,
            rate: None,
        };
        let error = self.finish_error(error, call, &IdentityRef::Anonymous, Some(exchange));
        Ok(Flow::Retry {
            error,
            beyond: 1,
            anonymous: true,
        })
    }

    fn judge_404(
        &self,
        call: &Call,
        decided: &Decided,
        prepared: &Prepared,
        slug: &Slug,
        failed: Failed<'_>,
    ) -> Result<Flow> {
        let error = failed.error;
        let as_ = prepared.as_.label().to_string();
        let root = repo_root(slug);
        let path = path_only(&call.path);
        let scope_next =
            classify::missing_scope(failed.headers).map(|scope| classify::scope_next(&scope));
        let next = scope_next
            .clone()
            .unwrap_or_else(|| format!("check the routing: agh whoami --for {slug}"));
        let sent = Tried::Sent {
            as_: as_.clone(),
            status: 404,
        };
        let rest = after_prefix(path, &root).unwrap_or(path);
        let rest = rest.trim_start_matches('/');
        if decided.covers_all_repos {
            let why = if rest.is_empty() {
                format!("as {as_}, {slug} answers 404, and {as_} covers every repository")
            } else {
                format!("{rest} is not found (as {as_}, which covers every repository)")
            };
            let next = scope_next.unwrap_or_else(|| Kind::NotFound.next_action().to_string());
            return Err(error
                .recast(Kind::NotFound, why)
                .with_tried(vec![sent])
                .with_next(next));
        }
        if rest.is_empty() {
            let why = format!(
                "as {as_}, {slug} answers 404: it does not exist, or this identity cannot see it"
            );
            return Err(error
                .recast(Kind::NotVisible, why)
                .with_tried(vec![sent])
                .with_next(next));
        }
        if !call.probe_404 {
            let why = format!(
                "as {as_}, {rest} answers 404, and the repository was not checked (not probed: --no-probe)"
            );
            return Err(error
                .recast(Kind::NotVisible, why)
                .with_tried(vec![sent])
                .with_next(next));
        }
        let probe_path = root.trim_start_matches('/').to_string();
        // This probe is one subrequest of the outer call: a transient failure returns
        // `Flow::Retry` to that call's shared budget below. NONE prevents a nested 19/200
        // budget while the composite anonymous read still retries as one operation.
        let probe = Call::get(root, call.route.clone())
            .probe_404(false)
            .follow_moved(false)
            .retry(Some(RetryPolicy::NONE))
            .deadlines(call.deadlines);
        let mut probe_sent = 0;
        match self.run_counted(&probe, decided, Reading::Whole, &mut probe_sent) {
            Ok(got) => {
                let fine_grained = decided
                    .cred
                    .as_ref()
                    .is_some_and(|cred| cred.kind() == TokenKind::FineGrained);
                let accepted = error
                    .github
                    .as_ref()
                    .and_then(|github| github.accepted_permissions.clone());
                let why = match accepted {
                    Some(accepted) if fine_grained => format!(
                        "{rest} is not found, or not permitted to this token (it needs {accepted})"
                    ),
                    _ => format!("{rest} is not found (the repository is visible as {as_})"),
                };
                let probe = Tried::Probe {
                    path: probe_path,
                    status: got.status,
                };
                Err(error
                    .recast(Kind::NotFound, why)
                    .with_tried(vec![sent, probe]))
            }
            Err(probed) if probed.http.as_ref().is_some_and(|h| h.status == 404) => {
                let why = format!(
                    "as {as_}, the repository answers 404, so {rest} cannot be judged absent"
                );
                let probe = Tried::Probe {
                    path: probe_path,
                    status: 404,
                };
                Err(error
                    .recast(Kind::NotVisible, why)
                    .with_tried(vec![sent, probe])
                    .with_next(next))
            }
            Err(probed) => Ok(Flow::Retry {
                error: probed,
                beyond: probe_sent,
                anonymous: prepared.as_ == IdentityRef::Anonymous,
            }),
        }
    }
}

#[cfg(all(test, unix))]
#[path = "client_write_tests.rs"]
mod write_tests;
