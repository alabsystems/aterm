// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh api` (design §10.2): one read of any path, or every page of it, and POST, PATCH, PUT
//! and DELETE writes to a repository's path under a permit.

use aterm_json::Value;

use crate::client::{Accept, Answered, Call, CallBody, Github, method_refusal};
use crate::error::{Error, IdentityRef, Result};
use crate::github::How;
use crate::journal::{IntentStore, Settled};
use crate::model::{UserObject, from_json};
use crate::page::{End, LinkScope, ListOpts};
use crate::policy::{Access, Permit, RouteRequest, RouteTarget};
use crate::retry::Idempotency;
use crate::slug::Slug;
use crate::wire::Method;
use crate::write::{Outcome, WriteOp};

/// `agh api`.
pub struct Raw<'a> {
    gh: &'a Github,
}

/// One `agh api` read request.
#[derive(Clone, Debug)]
pub struct RawGet {
    /// The method.
    pub method: Method,
    /// The path, starting with `/`, as typed.
    pub path: String,
    /// The query pairs.
    pub query: Vec<(String, String)>,
    /// The Accept header, when given.
    pub accept: Option<String>,
    /// How it is routed.
    pub route: RouteRequest,
    /// `--all`: every page.
    pub all: bool,
    /// `--include`: the status and headers too.
    pub include: bool,
    /// How the pages are read.
    pub opts: ListOpts,
}

/// What `api` answers.
#[derive(Clone, Debug, PartialEq)]
pub enum RawAnswer {
    /// A JSON body.
    Json(aterm_json::Value),
    /// `--all` over pages that are objects (printed one per line).
    Pages(Vec<aterm_json::Value>),
    /// A body that is not JSON.
    Bytes(Vec<u8>),
    /// `--include`.
    Included {
        /// The status.
        status: u16,
        /// The kept headers.
        headers: Vec<(String, String)>,
        /// The body as JSON, or its lossy text as a JSON string.
        body: aterm_json::Value,
    },
}

/// One `api` run.
#[derive(Clone, Debug, PartialEq)]
pub struct RawOutcome {
    /// The answer.
    pub answer: RawAnswer,
    /// Who it was read as.
    pub as_: IdentityRef,
    /// The requests sent.
    pub requests: u32,
    /// The pages read.
    pub pages: u32,
}

/// Percent-decodes `segment`, borrowing when no `%` is present.
fn decoded_segment(segment: &str) -> std::borrow::Cow<'_, str> {
    if segment.contains('%') {
        std::borrow::Cow::Owned(aterm_codec::pct::decode(segment))
    } else {
        std::borrow::Cow::Borrowed(segment)
    }
}

/// The path's segments, without its query, each percent-decoded: GitHub reads
/// `/repos/fixture%2Ddev/app` as `fixture-dev/app`, so the scope is judged on what it reads.
fn segments(path: &str) -> impl Iterator<Item = std::borrow::Cow<'_, str>> {
    path.split('?')
        .next()
        .unwrap_or_default()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(decoded_segment)
}

/// How a `Content-Type` labels a body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Media {
    /// `application/json`.
    Json,
    /// Another `+json` media type.
    PlusJson,
    /// Anything else, or no `Content-Type`.
    Other,
}

/// A `Content-Type`'s essence: before any `;`, trimmed; empty for none.
fn essence(content_type: Option<&str>) -> &str {
    content_type
        .and_then(|media| media.split(';').next())
        .map(str::trim)
        .unwrap_or_default()
}

impl Media {
    /// Classify `content_type` by its media essence.
    pub(crate) fn of(content_type: Option<&str>) -> Media {
        Media::of_essence(essence(content_type))
    }

    /// The label of an [`essence`].
    fn of_essence(essence: &str) -> Media {
        if essence.eq_ignore_ascii_case("application/json") {
            Media::Json
        } else if essence.len() > 5
            && essence.as_bytes()[essence.len() - 5..].eq_ignore_ascii_case(b"+json")
        {
            Media::PlusJson
        } else {
            Media::Other
        }
    }
}

/// Whether an answer is a file's bytes, whatever they look like: a raw or HTML rendering
/// (`application/vnd.github.raw`, `….html`, or either `+json`) of a repository's contents or
/// readme (`/repos/{o}/{r}/contents/…`, `/repos/{o}/{r}/readme…`, judged on the decoded path).
/// GitHub labels such bytes `+json` even when they are not JSON, and a file that happens to
/// parse is still the file (design §8.2: raw media types print bytes). Elsewhere GitHub uses
/// these types on JSON objects (an issue's `raw+json`), which stay "JSON when it parses".
fn is_file_bytes(essence: &str, path: &str) -> bool {
    let rendering = essence.eq_ignore_ascii_case("application/vnd.github.raw")
        || essence.eq_ignore_ascii_case("application/vnd.github.raw+json")
        || essence.eq_ignore_ascii_case("application/vnd.github.html")
        || essence.eq_ignore_ascii_case("application/vnd.github.html+json");
    if !rendering {
        return false;
    }
    let mut segs = segments(path);
    segs.next().as_deref() == Some("repos")
        && segs.next().is_some()
        && segs.next().is_some()
        && matches!(segs.next().as_deref(), Some("contents" | "readme"))
}

/// One answer as `api` prints it: its JSON, its bytes, or under `include` its status, headers
/// and body.
///
/// # Errors
///
/// CouldNotRun `Sub::Protocol` for an `application/json` body that is not JSON when
/// `strict_json` is true.
fn outcome_of(
    answered: Answered,
    include: bool,
    path: &str,
    strict_json: bool,
) -> Result<RawOutcome> {
    let essence = essence(answered.header("content-type"));
    let media = Media::of_essence(essence);
    let file_bytes = is_file_bytes(essence, path);
    let parsed = || match media {
        Media::Other => None,
        _ if file_bytes => None,
        Media::Json | Media::PlusJson if answered.body.is_empty() => None,
        Media::Json | Media::PlusJson => answered.json().ok(),
    };
    let answer = if include {
        let body = parsed()
            .unwrap_or_else(|| Value::String(String::from_utf8_lossy(&answered.body).into_owned()));
        RawAnswer::Included {
            status: answered.status,
            headers: answered.headers,
            body,
        }
    } else if strict_json && media == Media::Json && !answered.body.is_empty() {
        RawAnswer::Json(answered.json()?)
    } else {
        match parsed() {
            Some(value) => RawAnswer::Json(value),
            None => RawAnswer::Bytes(answered.body),
        }
    };
    Ok(RawOutcome {
        answer,
        as_: answered.as_,
        requests: answered.requests,
        pages: 1,
    })
}

/// `req.path`'s scope rules, `Raw::get`'s and `Raw::send`'s alike: no `#fragment`, which the
/// wire would cut, so the route would be decided for one path and another sent; no `.` or
/// `..` segment (typed or percent-encoded), which a normalising server would resolve off the
/// route.
fn path_rules(path: &str) -> Result<()> {
    if path.contains('#') {
        return Err(Error::usage(format!(
            "{path}: agh sends no #fragment (it is never sent, so the route would be judged on another path)"
        )));
    }
    crate::path::refuse_encoded_dot_segment("path", path.split('?').next().unwrap_or_default())
}

/// A write's 2xx answer as `api` prints it: `outcome_of`'s, except that an `application/json`
/// body that is not JSON is its bytes. GitHub answered a 2xx, so the write was done, and
/// nothing read after it may report it as not sent.
fn written_of(answered: Answered, include: bool, path: &str) -> RawOutcome {
    outcome_of(answered, include, path, false)
        .expect("outcome_of with strict_json=false is infallible")
}

/// The journal key of an `api` write of `method` to `path` (plan §7 body 7): `api {METHOD}
/// {path}`, the path canonical, so every spelling GitHub reads as one resource is one key and
/// `begin`'s open-intent rule holds across them. Each segment is percent-decoded and encoded
/// again (`path::segment`); a `/repos/{owner}/{repo}/…` path's owner and repository are
/// lowercased, as GitHub reads them; the query's pairs are sorted.
pub(crate) fn journal_key(method: Method, path: &str) -> String {
    let mut segs: Vec<std::borrow::Cow<'_, str>> = segments(path).collect();
    if segs.first().is_some_and(|first| first == "repos") {
        for seg in segs.iter_mut().skip(1).take(2) {
            if seg.bytes().any(|b| b.is_ascii_uppercase()) {
                seg.to_mut().make_ascii_lowercase();
            }
        }
    }
    let mut out = format!("api {} ", method.as_str());
    if segs.is_empty() {
        out.push('/');
    } else {
        for seg in &segs {
            out.push('/');
            out.push_str(&crate::path::segment(seg));
        }
    }
    if let Some((_, query)) = path.split_once('?') {
        let mut pairs: Vec<&str> = query.split('&').filter(|pair| !pair.is_empty()).collect();
        pairs.sort_unstable();
        for (i, pair) in pairs.iter().enumerate() {
            out.push(if i == 0 { '?' } else { '&' });
            out.push_str(pair);
        }
    }
    out
}

/// What a 2xx answer to an `api` POST created: its numeric `id` (0 for an object GitHub gives
/// none, such as a ref) and its `html_url`, else its `url`. `agh api` holds GitHub's whole
/// answer, so every 2xx is settled created.
fn created_of(answered: &Answered) -> Option<(u64, String)> {
    let value = answered.json().unwrap_or(Value::Null);
    let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
    let url = ["html_url", "url"]
        .iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .unwrap_or_default();
    Some((id, url.to_string()))
}

impl<'a> Raw<'a> {
    /// `agh api` over the client.
    pub(crate) const fn new(gh: &'a Github) -> Raw<'a> {
        Raw { gh }
    }

    /// The call `req` names, its 404 probed unless `no_probe` (`--no-probe`).
    fn call(req: &RawGet, no_probe: bool) -> Call {
        let mut call = Call::get(req.path.clone(), req.route.clone()).probe_404(!no_probe);
        call.method = req.method;
        for (key, value) in &req.query {
            call = call.query(key, value.clone());
        }
        match &req.accept {
            Some(media) => call.accept(Accept::Media(media.clone())),
            None => call,
        }
    }

    /// Execute `req`, reading one response or walking all pages when `req.all` is set; a HEAD is
    /// one response either way.
    ///
    /// # Errors
    ///
    /// A `req.method` other than GET and HEAD → Refused `policy.method_not_read`; Usage for a
    /// `req.path` holding a `#`, which the wire would cut, so the route would be decided for
    /// one path and another sent, or a `.` or `..` segment (typed or percent-encoded), which
    /// a normalising server would resolve off the route. Both before any request, `all`'s
    /// owner-id lookup included; else its calls' errors.
    pub fn get(&self, req: &RawGet) -> Result<RawOutcome> {
        self.get_with_end(req, false).map(|(outcome, _)| outcome)
    }

    /// Execute `req` and return the outcome paired with how pagination ended.
    ///
    /// # Errors
    ///
    /// Returns an error under the same conditions as [`Self::get`].
    pub(crate) fn get_with_end(&self, req: &RawGet, no_probe: bool) -> Result<(RawOutcome, End)> {
        if !req.method.is_read() {
            let repo = match &req.route.target {
                RouteTarget::Repo(slug) => Some(slug),
                _ => None,
            };
            return Err(method_refusal(req.method, repo));
        }
        path_rules(&req.path)?;
        if req.all && req.method != Method::Head {
            return self.all(req, no_probe);
        }
        let answered = self.gh.call(&Self::call(req, no_probe))?;
        let outcome = outcome_of(answered, req.include, &req.path, true)?;
        Ok((outcome, End::Complete))
    }

    /// `get` with `all`.
    fn all(&self, req: &RawGet, no_probe: bool) -> Result<(RawOutcome, End)> {
        let mut segs = segments(&req.path);
        let first = segs.next();
        let second = segs.next();
        let third = segs.next();
        let mut lookups = 0;
        let scope = match (first.as_deref(), second.as_deref(), third.as_deref()) {
            (Some("repos"), Some(owner), Some(repo)) => match Slug::new(owner, repo) {
                Ok(slug) => LinkScope::Repo(slug),
                Err(_) => LinkScope::Exact,
            },
            (Some("orgs" | "users"), Some(owner), _) => {
                let mut route = req.route.clone();
                route.target = RouteTarget::Owner(owner.to_string());
                let path = format!("/users/{}", crate::path::segment(owner));
                let answered = self.gh.call(&Call::get(path, route).probe_404(!no_probe))?;
                lookups = answered.requests;
                let user: UserObject = from_json(&answered.json()?)?;
                LinkScope::Owner { id: user.id }
            }
            _ => LinkScope::Exact,
        };
        let listing =
            crate::page::walk_pages(self.gh, Self::call(req, no_probe), &req.opts, &scope)?;
        let answer = if listing.items.iter().all(Value::is_array) {
            let items = listing
                .items
                .into_iter()
                .flat_map(|page| match page {
                    Value::Array(items) => items,
                    other => vec![other],
                })
                .collect();
            RawAnswer::Json(Value::Array(items))
        } else {
            RawAnswer::Pages(listing.items)
        };
        let outcome = RawOutcome {
            answer,
            as_: listing.as_,
            requests: listing.requests.saturating_add(lookups),
            pages: listing.pages,
        };
        Ok((outcome, listing.end))
    }
}

/// One `agh api` write.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RawSend {
    /// POST, PATCH, PUT or DELETE.
    pub method: Method,
    /// The path, starting with `/`, as typed.
    pub path: String,
    /// The JSON body `-f` and `-F` built, or none.
    pub body: Option<aterm_json::Value>,
    /// The Accept header, when given.
    pub accept: Option<String>,
    /// How it is routed: always `Access::Write`.
    pub route: RouteRequest,
    /// `--include`.
    pub include: bool,
}

impl RawSend {
    /// A write of `method` to `path`, routed by `route` (its access set to Write).
    #[must_use]
    pub fn new(method: Method, path: &str, route: RouteRequest) -> RawSend {
        RawSend {
            method,
            path: path.to_string(),
            body: None,
            accept: None,
            route: RouteRequest {
                access: Access::Write,
                ..route
            },
            include: false,
        }
    }
}

impl Raw<'_> {
    /// One write, sent once and never retried. A POST is one-shot and journaled
    /// (`WriteOp::Api`, key `journal_key(POST, path)`: `api POST {path}`, its path canonical)
    /// through `intents`; PATCH, PUT and DELETE are not journaled and `intents` is not called.
    /// A 2xx is `Outcome::Done` whatever its body: one that is not the JSON it is labelled is
    /// its bytes.
    ///
    /// # Errors
    ///
    /// Usage for a read method (`Raw::get` takes it); `Raw::get`'s path scope rules; Usage for
    /// a path whose repository is not the permit's, before anything is journaled or loaded;
    /// the pipeline's errors.
    pub fn send(
        &self,
        permit: &Permit,
        req: &RawSend,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<RawOutcome>> {
        if cfg!(not(unix)) {
            return Err(crate::write::unix_only());
        }
        if req.method.is_read() {
            return Err(Error::usage(format!(
                "{} is a read: Raw::get takes it",
                req.method.as_str()
            )));
        }
        path_rules(&req.path)?;
        let (call, slug) = Self::write_call(req)?;
        // A path the permit does not cover is Usage before anything is journaled or loaded:
        // the POST's draft below names the path's repository, and the journal it begins in is
        // the caller's.
        crate::client::write_target(&call, permit)?;
        let include = req.include;
        let path = req.path.as_str();
        if req.method == Method::Post {
            let Some(slug) = slug else {
                // No permit covers an Unscoped target, so `write_target` refused it above;
                // the pipeline refuses it again, Usage, and nothing is sent or journaled.
                return self
                    .gh
                    .call_write(&call, permit, &How::write())
                    .map(|answered| Outcome::Done(written_of(answered, include, path)));
            };
            let key = journal_key(Method::Post, path);
            let draft = self
                .gh
                .intent_draft(WriteOp::Api, &slug, &call, permit, &key)?;
            let outcome = self
                .gh
                .one_shot(&call, permit, &draft, intents, &created_of)?;
            return Ok(match outcome {
                Outcome::Done(answered) => Outcome::Done(written_of(answered, include, path)),
                Outcome::NeverSent(error) => Outcome::NeverSent(error),
                Outcome::Refused(error) => Outcome::Refused(error),
                Outcome::Unknown { intent, error } => Outcome::Unknown { intent, error },
            });
        }
        // Not journaled, but classified by the one settle rule (plan §7 body 6); a 2xx is
        // done whatever its body, so `created` is given.
        let result = self.gh.call_write(&call, permit, &How::write());
        let settled = crate::write::settle_table(&result, Some((0, String::new())));
        match (settled, result) {
            (_, Ok(answered)) => Ok(Outcome::Done(written_of(answered, include, path))),
            (Settled::NeverSent, Err(error)) => Ok(Outcome::NeverSent(error)),
            (Settled::Refused { .. }, Err(error)) => Ok(Outcome::Refused(error)),
            (_, Err(error)) => Err(error),
        }
    }

    /// The write `req` names, OneShot and never retried, and its repository: a
    /// `/repos/{owner}/{repo}/…` path is `RouteTarget::Repo`, and every other path is
    /// Unscoped, which no permit covers, so the pipeline refuses it before anything is sent.
    fn write_call(req: &RawSend) -> Result<(Call, Option<Slug>)> {
        let mut segs = segments(&req.path);
        let first = segs.next();
        let second = segs.next();
        let third = segs.next();
        let (target, slug) = match (first.as_deref(), second.as_deref(), third.as_deref()) {
            (Some("repos"), Some(owner), Some(repo)) => match Slug::new(owner, repo) {
                Ok(slug) => (RouteTarget::Repo(slug.clone()), Some(slug)),
                Err(_) => (RouteTarget::Unscoped, None),
            },
            _ => (RouteTarget::Unscoped, None),
        };
        let route = RouteRequest {
            target,
            access: Access::Write,
            ..req.route.clone()
        };
        let body = match &req.body {
            Some(value) => CallBody::Json(
                aterm_json::to_vec(value)
                    .map_err(|e| Error::usage(format!("the -f/-F body is not JSON: {e}")))?,
            ),
            None => CallBody::Empty,
        };
        let call = Call::write(
            req.method,
            req.path.clone(),
            route,
            Idempotency::OneShot,
            body,
        );
        let call = match &req.accept {
            Some(media) => call.accept(Accept::Media(media.clone())),
            None => call,
        };
        Ok((call, slug))
    }
}
