// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! HTTP client for routed, rate-recorded, and retried GitHub API calls.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::classify;
use crate::error::{Error, HttpRef, IdentityRef, Kind, Result, Sub, Tried};
use crate::fx::Fx;
use crate::github::repo_root;
use crate::identity::{self, Credential, EnvSource, LoadError, ProcessEnv, TokenKind};
use crate::policy::{
    Access, AnonymousWhy, Decision, Presence, Refusal, RefusalCode, RouteRequest, RouteTarget,
    Router,
};
use crate::rate::{RateBook, RateSnapshot, book_key, resource_for};
use crate::redact::Redactor;
use crate::retry::{Idempotency, RetryPolicy, should_retry};
use crate::slug::Slug;
use crate::wire::door::AuthHeader;
use crate::wire::http::HttpTransport;
use crate::wire::{
    Deadlines, Endpoints, Method, Origin, OriginRole, Transport, WireBody, WireError, WireRequest,
    WireResponse, header_in, hop, path_only,
};

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

/// A call's default body limit: 32 MiB.
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
    /// Hand back any status as an `Answered`, unclassified, with no retry, probe, move, or hop.
    pub answer_any: bool,
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
        }
    }

    /// As `get`, with HEAD.
    pub fn head(path: impl Into<String>, route: RouteRequest) -> Call {
        Call {
            method: Method::Head,
            ..Call::get(path, route)
        }
    }

    /// The same call on `origin`.
    #[inline]
    #[must_use]
    pub fn origin(mut self, origin: OriginRole) -> Call {
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
    pub fn limit(mut self, limit: u64) -> Call {
        self.limit = limit;
        self
    }

    /// The same call with `deadlines`.
    #[inline]
    #[must_use]
    pub fn deadlines(mut self, deadlines: Deadlines) -> Call {
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
    pub fn probe_404(mut self, probe: bool) -> Call {
        self.probe_404 = probe;
        self
    }

    /// The same call following a 301 or not.
    #[inline]
    #[must_use]
    pub fn follow_moved(mut self, follow: bool) -> Call {
        self.follow_moved = follow;
        self
    }

    /// The same call hopping or not (for `call_stream`; see the field).
    #[inline]
    #[must_use]
    pub fn hop(mut self, hop: bool) -> Call {
        self.hop = hop;
        self
    }

    /// The same call with its own retry policy.
    #[inline]
    #[must_use]
    pub fn retry(mut self, policy: Option<RetryPolicy>) -> Call {
        self.retry = policy;
        self
    }

    /// The same call answering any status or not.
    #[inline]
    #[must_use]
    pub fn answer_any(mut self, any: bool) -> Call {
        self.answer_any = any;
        self
    }

    /// Encoded path and query string.
    #[must_use]
    pub fn path_and_query(&self) -> String {
        if self.query.is_empty() {
            self.path.clone()
        } else {
            let join = if self.path.contains('?') { '&' } else { '?' };
            format!("{}{join}{}", self.path, crate::path::query(&self.query))
        }
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
    ///
    /// # Errors
    ///
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
}

impl Meter {
    /// The last request's identity as a label.
    #[inline]
    pub(crate) fn label(&self) -> &str {
        self.as_.as_ref().map_or("anonymous", IdentityRef::label)
    }
}

/// What a client shares between its clones.
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
    /// Each profile's load, once per process.
    credentials: Mutex<BTreeMap<String, std::result::Result<Arc<Credential>, LoadError>>>,
    /// Each repository's numeric id, by `Slug::key`: the only memo (design §14).
    ids: Mutex<BTreeMap<String, u64>>,
    meter: Mutex<Meter>,
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
        }
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

    /// The `User-Agent` caller: `agh/{VERSION} ({caller})`.
    #[must_use]
    pub fn caller(mut self, caller: &str) -> GithubBuilder {
        self.caller = caller.to_string();
        self
    }

    /// The retry policy.
    #[must_use]
    pub fn retry(mut self, policy: RetryPolicy) -> GithubBuilder {
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
    pub fn backstop(mut self, backstop: Option<Duration>) -> GithubBuilder {
        self.backstop = backstop;
        self
    }

    /// The client.
    #[must_use]
    pub fn build(self) -> Github {
        let transport = self
            .transport
            .unwrap_or_else(|| Arc::new(HttpTransport::new(self.env.clone())));
        let user_agent = format!("agh/{} ({})", crate::VERSION, self.caller);
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
                credentials: Mutex::new(BTreeMap::new()),
                ids: Mutex::new(BTreeMap::new()),
                meter: Mutex::new(Meter::default()),
            }),
        }
    }
}

impl Github {
    /// Execute a call and read its body whole.
    ///
    /// # Errors
    ///
    /// Returns the routed, classified error, or refuses non-GET/HEAD or hopped calls.
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
    ///
    /// # Errors
    ///
    /// As `call`.
    pub fn call_stream(&self, call: &Call) -> Result<Streamed> {
        method_gate(call)?;
        let decided = self.route(&call.route)?;
        self.run(call, &decided, Reading::Stream)
            .map(Got::into_streamed)
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

    /// `agh/{VERSION} ({caller})`.
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
        let loaded = credentials.entry(profile.to_string()).or_insert_with(|| {
            let loaded = identity::load(row, &*self.inner.env).map(Arc::new);
            if let Ok(cred) = &loaded {
                self.inner.redactor.learn(cred.expose());
            }
            loaded
        });
        match loaded {
            Ok(_) => Presence::Present,
            Err(e) => e.presence(),
        }
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
    ///
    /// # Errors
    ///
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
            .get(&slug.key())
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
            .insert(slug.key(), id);
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
                credentials: Mutex::new(BTreeMap::new()),
                ids: Mutex::new(BTreeMap::new()),
                meter: Mutex::new(Meter::default()),
            }),
        }
    }
}

impl std::fmt::Debug for Github {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Github({})", self.inner.caller)
    }
}

/// The most bytes of a failed answer's body read to classify it.
const HEAD_MAX: u64 = 64 * 1024;

/// Whether the body is read whole inside the door (`call`) or handed back unread
/// (`call_stream`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    Whole,
    Stream,
}

/// A successful answer's body, in the form its pipeline reads it.
enum Body {
    Whole(Vec<u8>),
    Stream(Box<dyn Read + Send>),
}

/// A streamed 2xx body held to the call's limit (plan §8 step 9).
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

/// A successful answer, before it becomes an `Answered` or a `Streamed`.
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
    /// The answer: `whole` is a `Reading::Whole` body, already read and held to the call's
    /// limit (`Prepared::held`), and taken as it is; with none, the body streams.
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

    /// `call`'s answer.
    fn into_answered(self) -> Result<Answered> {
        let body = match self.body {
            Body::Whole(bytes) => bytes,
            Body::Stream(mut reader) => {
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes).map_err(|e| {
                    Error::could_not_run(format!("the answer could not be read: {e}"))
                        .with_sub(Sub::Local)
                })?;
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

    /// `call_stream`'s answer.
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

/// What the router decided, ready to send.
struct Decided {
    identity: IdentityRef,
    cred: Option<Arc<Credential>>,
    covers_all_repos: bool,
}

/// One request, built once and sent on each attempt.
struct Prepared {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    auth: Option<AuthHeader>,
    /// Who the request is sent as: the decided identity when the door gave it a
    /// credential, else anonymous (the web and CDN origins, whatever was routed).
    as_: IdentityRef,
    /// The call's limit.
    limit: u64,
    reading: Reading,
    deadlines: Deadlines,
}

impl Prepared {
    /// The wire request this attempt sends.
    fn request(&self) -> WireRequest<'_> {
        WireRequest {
            method: self.method,
            url: self.url.clone(),
            headers: self.headers.clone(),
            auth: self.auth.as_ref(),
            body: WireBody::Empty,
            limit: self.limit.max(HEAD_MAX),
            deadlines: self.deadlines,
        }
    }

    /// An answer the call takes, held to the call's limit: a whole body longer than `limit`
    /// is the wire's `TooLarge` at `Answered`, exactly as a wire read under that limit would
    /// have answered it; a stream is held as it is read (`Got::of`).
    fn held(&self, response: &mut WireResponse) -> std::result::Result<Option<Vec<u8>>, WireError> {
        match self.reading {
            Reading::Whole => response.read_body(self.limit).map(Some),
            Reading::Stream => Ok(None),
        }
    }
}

/// What a failed answer leaves to the send loop.
enum Flow {
    /// A later step (a move, a hop) produced the answer.
    Done(Box<Got>),
    /// The error, for `should_retry`, and the requests a later step (the hop) sent beyond
    /// the attempt's own.
    Retry(Error, u32),
}

/// The exchange a classified error came from.
struct Exchange<'a> {
    status: u16,
    headers: &'a [(String, String)],
    rate: Option<RateSnapshot>,
}

/// The statuses that are the answer: 2xx and 304 for `call` (plan §8 step 4), 2xx only for
/// `call_stream` (a `Streamed` is a 2xx, plan §4.6.17).
fn is_success(status: u16, reading: Reading) -> bool {
    (200..300).contains(&status) || (status == 304 && reading == Reading::Whole)
}

/// The repository a route is about.
fn repo_of(route: &RouteRequest) -> Option<&Slug> {
    match &route.target {
        RouteTarget::Repo(slug) => Some(slug),
        _ => None,
    }
}

/// A redirect agh does not follow: `error` (the 3xx's, its status, request id and rate kept)
/// recast as CouldNotRun `Sub::Protocol` with `why` and the kind's next.
fn protocol_error(error: Error, why: String) -> Error {
    error
        .recast(Kind::CouldNotRun, why)
        .with_sub(Sub::Protocol)
        .with_next(Kind::CouldNotRun.next_action())
}

/// What follows `prefix` in `path`, matched ASCII case-insensitively, when the prefix ends at
/// a segment boundary.
fn after_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    crate::wire::strip_prefix_ci(path, prefix)
        .filter(|rest| rest.is_empty() || rest.starts_with(['/', '?']))
}

/// Retarget `call` to a moved repository's new slug.
pub(crate) fn retarget(call: &mut Call, old: &Slug, new: &Slug) {
    if let Some(rest) = after_prefix(&call.path, &repo_root(old)).map(str::to_string) {
        call.path = format!("{}{rest}", repo_root(new));
    }
    if repo_of(&call.route).is_some_and(|slug| slug.key() == old.key()) {
        call.route.target = RouteTarget::Repo(new.clone());
    }
}

/// Build the refusal error for write methods in M1.
pub(crate) fn method_refusal(method: Method, repo: Option<&Slug>) -> Error {
    Refusal {
        code: RefusalCode::MethodNotRead,
        why: format!(
            "{} is a write, and agh M1 sends only GET and HEAD",
            method.as_str()
        ),
        next: "writes arrive in M2: use gh for now".to_string(),
        tried: Vec::new(),
    }
    .into_error(repo)
}

/// Step 0: in M1 only GET and HEAD are routed, loaded or sent.
fn method_gate(call: &Call) -> Result<()> {
    if call.method.is_read() {
        Ok(())
    } else {
        Err(method_refusal(call.method, repo_of(&call.route)))
    }
}

/// A failed answer's kept headers and the first 64 KiB of its body.
fn head_of(response: WireResponse) -> (Vec<(String, String)>, Vec<u8>) {
    let WireResponse { headers, body, .. } = response;
    let mut head = Vec::new();
    body.take(HEAD_MAX)
        .read_to_end(&mut head)
        .unwrap_or_default();
    (headers, head)
}

/// The id of a `Location` on the api origin of the form `/repositories/{digits}…`.
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
    /// The decided profile's cached credential.
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

    /// Step 1: the route.
    fn route(&self, route: &RouteRequest) -> Result<Decided> {
        match self.decide(route) {
            Decision::Send { profile, .. } => {
                let cred = self.credential(&profile).ok_or_else(|| {
                    Error::new(
                        Kind::Unauthenticated,
                        format!("the credential of {profile} did not load"),
                    )
                })?;
                let covers_all_repos = self
                    .inner
                    .router
                    .profile(&profile)
                    .is_some_and(|p| p.covers_all_repos);
                Ok(Decided {
                    identity: IdentityRef::Profile {
                        name: profile,
                        fingerprint: cred.fingerprint().to_string(),
                    },
                    cred: Some(cred),
                    covers_all_repos,
                })
            }
            Decision::Anonymous { why } => {
                if why != AnonymousWhy::Asked {
                    self.lock_meter().fell_to_anonymous = true;
                }
                Ok(Decided {
                    identity: IdentityRef::Anonymous,
                    cred: None,
                    covers_all_repos: false,
                })
            }
            Decision::Refuse(refusal) => Err(refusal.into_error(repo_of(route))),
        }
    }

    /// The meter, locked.
    fn lock_meter(&self) -> std::sync::MutexGuard<'_, Meter> {
        self.inner
            .meter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Step 3: the headers, the credential, the URL and the deadlines.
    fn prepare(&self, call: &Call, decided: &Decided, reading: Reading) -> Prepared {
        let endpoints = &self.inner.endpoints;
        let mut headers = vec![
            ("Accept".to_string(), call.accept.header_value().to_string()),
            ("User-Agent".to_string(), self.inner.user_agent.clone()),
        ];
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
        let as_ = if auth.is_some() {
            decided.identity.clone()
        } else {
            IdentityRef::Anonymous
        };
        Prepared {
            method: call.method,
            url: endpoints.url(call.origin, &call.path_and_query()),
            headers,
            auth,
            as_,
            limit: call.limit,
            reading,
            deadlines,
        }
    }

    /// One round trip through the seam, counted in the meter.
    fn send(&self, prepared: &Prepared) -> Result<std::result::Result<WireResponse, WireError>> {
        {
            let mut meter = self.lock_meter();
            meter.requests = meter.requests.saturating_add(1);
            meter.as_ = Some(prepared.as_.clone());
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

    /// Step 8: the exchange, the identity (who the request was sent as) and the repository on
    /// an error.
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

    /// Steps 2 to 7 for one routed call: the rate book, the send loop, the classification,
    /// moves, the hop and the 404 rules.
    fn run(&self, call: &Call, decided: &Decided, reading: Reading) -> Result<Got> {
        let mut sent = 0;
        self.run_counted(call, decided, reading, &mut sent)
    }

    /// `run`, leaving in `sent` the requests it sent, whatever it answers: the 404 probe's
    /// failure is retried as the call's own (step 4), with its request counted.
    fn run_counted(
        &self,
        call: &Call,
        decided: &Decided,
        reading: Reading,
        sent: &mut u32,
    ) -> Result<Got> {
        let prepared = self.prepare(call, decided, reading);
        let key = book_key(&decided.identity);
        let booked = call.origin == OriginRole::Api && self.inner.rate_book.enabled();
        let free = path_only(&call.path) == "/rate_limit";
        if booked && !free {
            let now = self.inner.fx.wall_secs()?;
            let resource = resource_for(&call.path);
            if let Some(refused) = self.inner.rate_book.refuse_before_send(&key, resource, now) {
                return Err(self.finish_error(refused, call, &prepared.as_, None));
            }
        }
        let policy = call.retry.unwrap_or(self.inner.retry);
        let mut requests = 0u32;
        let mut attempt = 1u32;
        let mut secondary_retried = false;
        loop {
            requests = requests.saturating_add(1);
            *sent = requests;
            let failure = match self.send(&prepared)? {
                Err(wire) => self.finish_error(
                    classify::wire(&wire, call.method, call.idempotency),
                    call,
                    &prepared.as_,
                    None,
                ),
                Ok(mut response) => {
                    let rate = RateSnapshot::from_headers(&response.headers);
                    if let Some(snapshot) = &rate {
                        self.lock_meter().rate = Some(snapshot.clone());
                        if booked {
                            self.inner.rate_book.record(&key, snapshot);
                        }
                    }
                    if call.answer_any || is_success(response.status, reading) {
                        match prepared.held(&mut response) {
                            Ok(whole) => {
                                return Ok(Got::of(
                                    response,
                                    whole,
                                    &prepared.as_,
                                    requests,
                                    rate,
                                    call.limit,
                                ));
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
                        let classified = classify::answer(
                            status,
                            &headers,
                            &head,
                            call.method,
                            call.idempotency,
                        )
                        .unwrap_or_else(|| Error::protocol(format!("GitHub answered {status}")));
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
                            Flow::Done(got) => return Ok(*got),
                            Flow::Retry(error, beyond) => {
                                requests = requests.saturating_add(beyond);
                                *sent = requests;
                                error
                            }
                        }
                    }
                }
            };
            let Some(pause) = should_retry(
                &policy,
                call.idempotency,
                attempt,
                &failure,
                secondary_retried,
            ) else {
                return Err(failure);
            };
            if failure.sub == Sub::Secondary {
                secondary_retried = true;
            }
            self.inner.fx.sleep(pause)?;
            attempt = attempt.saturating_add(1);
        }
    }
}

/// A failed answer, classified, for steps 5 to 7.
struct Failed<'a> {
    status: u16,
    headers: &'a [(String, String)],
    error: Error,
    /// Requests sent for this call so far.
    requests: u32,
}

impl Github {
    /// Steps 5 to 7: a move, the hop, the 404 rules; any other error goes to the retry rule.
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
                            .map(|got| Flow::Done(Box::new(got)))
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
                None => Ok(Flow::Retry(failed.error, 0)),
            },
            _ => Ok(Flow::Retry(failed.error, 0)),
        }
    }

    /// Step 5: follow a 301 to `/repositories/{id}` once.
    fn follow_move(
        &self,
        call: &Call,
        id: u64,
        failed: Failed<'_>,
        reading: Reading,
    ) -> Result<Got> {
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
        .retry(call.retry)
        .deadlines(call.deadlines);
        let looked = match self.call(&lookup) {
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
            Err(error) if error.repo.is_none() => return Err(error.with_repo(old.to_string())),
            Err(error) => return Err(error),
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
        let decided = self.route(&moved.route)?;
        let mut got = self.run(&moved, &decided, reading)?;
        got.requests = got
            .requests
            .saturating_add(failed.requests)
            .saturating_add(looked.requests);
        got.moved = Some((old, new));
        self.lock_meter().moved.clone_from(&got.moved);
        Ok(got)
    }

    /// Step 6: the one hop, sent with no credential (`hop::rebuild`).
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
            let mut meter = self.lock_meter();
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
                return Ok(Flow::Retry(error, 1));
            }
        };
        let status = response.status;
        if is_success(status, Reading::Stream) {
            let mut got = Got::of(response, None, &prepared.as_, requests, None, call.limit);
            got.hopped = true;
            return Ok(Flow::Done(Box::new(got)));
        }
        let (headers, head) = head_of(response);
        let error = if (300..400).contains(&status) {
            Error::protocol("a second redirect")
        } else {
            classify::answer(status, &headers, &head, call.method, call.idempotency)
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
        Ok(Flow::Retry(error, 1))
    }

    /// Step 7: a 404 on a `Repo(slug)` target (design §7.3).
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
        let probe = Call::get(root.clone(), call.route.clone())
            .probe_404(false)
            .follow_moved(false)
            .retry(Some(RetryPolicy::NONE))
            .deadlines(call.deadlines);
        let probe_path = root.trim_start_matches('/').to_string();
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
            Err(probed) => Ok(Flow::Retry(probed, probe_sent)),
        }
    }
}
