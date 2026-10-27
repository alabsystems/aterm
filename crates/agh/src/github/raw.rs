// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh api` (design §10.2): one read of any path, or every page of it.

use aterm_json::Value;

use crate::client::{Accept, Call, Github, method_refusal};
use crate::error::{Error, IdentityRef, Result};
use crate::journal::IntentStore;
use crate::model::{UserObject, from_json};
use crate::page::{End, LinkScope, ListOpts};
use crate::policy::{Access, Permit, RouteRequest, RouteTarget};
use crate::slug::Slug;
use crate::wire::Method;
use crate::write::Outcome;

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

/// The path's segments, without its query, each percent-decoded: GitHub reads
/// `/repos/fixture%2Ddev/app` as `fixture-dev/app`, so the scope is judged on what it reads.
fn segments(path: &str) -> Vec<String> {
    path.split('?')
        .next()
        .unwrap_or_default()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(aterm_codec::pct::decode)
        .collect()
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

/// A `Content-Type`'s essence: before any `;`, trimmed, ASCII lowercase; empty for none.
fn essence(content_type: Option<&str>) -> String {
    content_type
        .and_then(|media| media.split(';').next())
        .map(|media| media.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

impl Media {
    /// Classify `content_type` by its media essence.
    pub(crate) fn of(content_type: Option<&str>) -> Media {
        Media::of_essence(&essence(content_type))
    }

    /// The label of an [`essence`].
    fn of_essence(essence: &str) -> Media {
        if essence == "application/json" {
            Media::Json
        } else if essence.ends_with("+json") {
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
    let rendering = matches!(
        essence,
        "application/vnd.github.raw"
            | "application/vnd.github.raw+json"
            | "application/vnd.github.html"
            | "application/vnd.github.html+json"
    );
    let segs = segments(path);
    let file = matches!(
        segs.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["repos", _, _, "contents", ..] | ["repos", _, _, "readme", ..]
    );
    rendering && file
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
        if req.path.contains('#') {
            return Err(Error::usage(format!(
                "{}: agh sends no #fragment (it is never sent, so the route would be judged on another path)",
                req.path
            )));
        }
        crate::path::refuse_encoded_dot_segment(
            "path",
            req.path.split('?').next().unwrap_or_default(),
        )?;
        if req.all && req.method != Method::Head {
            return self.all(req, no_probe);
        }
        let answered = self.gh.call(&Self::call(req, no_probe))?;
        let essence = essence(answered.header("content-type"));
        let media = Media::of_essence(&essence);
        let file_bytes = is_file_bytes(&essence, &req.path);
        let parsed = || match media {
            Media::Other => None,
            _ if file_bytes => None,
            Media::Json | Media::PlusJson if answered.body.is_empty() => None,
            Media::Json | Media::PlusJson => answered.json().ok(),
        };
        let answer = if req.include {
            let body = parsed().unwrap_or_else(|| {
                Value::String(String::from_utf8_lossy(&answered.body).into_owned())
            });
            RawAnswer::Included {
                status: answered.status,
                headers: answered.headers.clone(),
                body,
            }
        } else if media == Media::Json && !answered.body.is_empty() {
            RawAnswer::Json(answered.json()?)
        } else {
            match parsed() {
                Some(value) => RawAnswer::Json(value),
                None => RawAnswer::Bytes(answered.body.clone()),
            }
        };
        let outcome = RawOutcome {
            answer,
            as_: answered.as_,
            requests: answered.requests,
            pages: 1,
        };
        Ok((outcome, End::Complete))
    }

    /// `get` with `all`.
    fn all(&self, req: &RawGet, no_probe: bool) -> Result<(RawOutcome, End)> {
        let segs = segments(&req.path);
        let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
        let mut lookups = 0;
        let scope = match segs.as_slice() {
            ["repos", owner, repo, ..] => match Slug::new(owner, repo) {
                Ok(slug) => LinkScope::Repo(slug),
                Err(_) => LinkScope::Exact,
            },
            ["orgs" | "users", owner, ..] => {
                let mut route = req.route.clone();
                route.target = RouteTarget::Owner((*owner).to_string());
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
    /// (`WriteOp::Api`, key `api POST {path}`) through `intents`; PATCH, PUT and DELETE are
    /// not journaled and `intents` is not called.
    ///
    /// # Errors
    ///
    /// Usage for a read method (`Raw::get` takes it); `Raw::get`'s path scope rules; the
    /// pipeline's errors.
    pub fn send(
        &self,
        permit: &Permit,
        req: &RawSend,
        intents: &dyn IntentStore,
    ) -> Result<Outcome<RawOutcome>> {
        let _ = (self.gh, permit, req, intents);
        Err(Error::not_yet("api writes", "WP3"))
    }
}
