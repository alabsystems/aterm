// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh api` (design §10.2): one read of any path, or every page of it.

use aterm_json::Value;

use crate::client::{Accept, Call, Github, method_refusal};
use crate::error::{Error, IdentityRef, Result};
use crate::model::{UserObject, from_json};
use crate::page::{End, LinkScope, ListOpts};
use crate::policy::{RouteRequest, RouteTarget};
use crate::slug::Slug;
use crate::wire::Method;

/// `agh api`.
pub struct Raw<'a> {
    gh: &'a Github,
}

/// One `agh api` request: a read. `Raw::get` refuses a `method` other than GET and HEAD. M2
/// adds `Raw::send` for writes and leaves `RawGet` as it is.
#[derive(Clone, Debug)]
pub struct RawGet {
    /// The method.
    pub method: Method,
    /// The path, starting with `/`, as typed (the CLI encodes nothing further).
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
    /// `application/json`: what GitHub sends for an object or an array.
    Json,
    /// Another `+json` type. GitHub labels bytes that are not JSON this way too (a raw file
    /// is `application/vnd.github.raw+json`, an HTML rendering `….html+json`), so such a body
    /// is JSON only when it parses.
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
    /// The label of `content_type`, judged on its essence (before any `;`, trimmed, ASCII
    /// case-insensitive), never on a substring.
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
    pub(crate) fn new(gh: &'a Github) -> Raw<'a> {
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

    /// Send `req`. Without `all`: one call; `include` → `Included` (a non-empty body that
    /// parses as JSON under a JSON content type, else its lossy text); an `application/json`
    /// body → `Json`; a raw or HTML rendering of a contents or readme path → `Bytes`, even
    /// when it parses; another `+json` body → `Json` when it parses, else `Bytes`; anything
    /// else (an empty body included) → `Bytes`. With `all`: every page (`page::walk_pages`), scoped
    /// by the path (`/repos/{o}/{r}/…` → `Repo`, `/orgs/{o}/…` and `/users/{o}/…` → `Owner`,
    /// with the owner's id read first, else `Exact`); arrays are concatenated into `Json`, any
    /// other pages are `Pages`. A HEAD has no pages, so it is one call with or without `all`.
    ///
    /// # Errors
    ///
    /// A `req.method` other than GET and HEAD → Refused `policy.method_not_read`, before any
    /// request, `all`'s owner-id lookup included; Usage for a `req.path` holding a `#`, which
    /// the wire would cut, so the route would be decided for one path and another sent, or a
    /// `.` or `..` segment (typed or percent-encoded), which a normalising server would
    /// resolve off the route (`all`'s owner-id lookup included); else its calls' errors.
    /// The 404 of a `/repos/{o}/{r}/…` path is probed.
    pub fn get(&self, req: &RawGet) -> Result<RawOutcome> {
        self.get_with_end(req, false).map(|(outcome, _)| outcome)
    }

    /// `get`, with how its walk ended: `End::Complete` for one call; under `all`, the walk's
    /// own end (`Limited` at `req.opts.limit` pages, `Capped` at the page cap under
    /// `partial`). `RawOutcome` carries no end, so `agh api` reads it here. Crate-private:
    /// the public API of `Raw` is plan §4.6.20's `get`. `no_probe` (`--no-probe`) leaves a 404 unprobed
    /// (NotVisible), which the frozen `RawGet` has no field for.
    ///
    /// # Errors
    ///
    /// As `get`.
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
        // Judged on the whole decoded path, so a `..` behind an encoded `/` is refused too: a
        // server that normalised it would leave the route.
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
            // A body labelled `application/json` that does not parse is GitHub's fault.
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
