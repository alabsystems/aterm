// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fake GitHub route handlers, credential and rate headers, ETags, and fault injection.

use std::time::Duration;

use aterm_json::{Value, json};

use crate::state::{File, Latest, Release, Repo, State};
use crate::{CompareSpec, DigestSpec, FakeOrigin, Fault, ReleaseSpec, Seen};

/// GitHub's documentation link, on every error body.
const DOCS: &str = "https://docs.github.com/rest";
/// The reset every rate header names.
const RESET: u64 = 1_760_003_600;
/// JSON media type used in fake responses.
const JSON: &str = "application/json; charset=utf-8";
/// The bytes media type.
const OCTET: &str = "application/octet-stream";
/// Media type returned by GitHub for raw file contents.
const RAW_FILE: &str = "application/vnd.github.raw+json; charset=utf-8";
/// GitHub's page size when a request names none.
const DEFAULT_PER_PAGE: usize = 30;
/// The releases listing's result cap.
const RELEASES_RESULT_CAP: usize = 1000;

/// One answer: status, headers and body.
pub(crate) struct Answer {
    /// The status.
    pub(crate) status: u16,
    /// The headers, `Content-Length` and `Connection` aside.
    pub(crate) headers: Vec<(String, String)>,
    /// The body (a HEAD's is dropped when written, after its length is taken).
    pub(crate) body: Vec<u8>,
}

impl Answer {
    /// A JSON answer.
    fn json(status: u16, value: &Value) -> Answer {
        Answer {
            status,
            headers: vec![("Content-Type".into(), JSON.into())],
            body: value.to_string().into_bytes(),
        }
    }

    /// A plain-text answer.
    fn text(status: u16, text: &str) -> Answer {
        Answer {
            status,
            headers: vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
            body: text.as_bytes().to_vec(),
        }
    }

    /// GitHub's error body: `message`, `documentation_url` and `status`.
    fn error(status: u16, message: &str) -> Answer {
        Answer::json(
            status,
            &json!({"message": message, "documentation_url": DOCS, "status": status.to_string()}),
        )
    }

    /// The 404 every hidden or missing thing answers.
    fn not_found() -> Answer {
        Answer::error(404, "Not Found")
    }

    /// A redirect to `location`.
    fn redirect(status: u16, location: String) -> Answer {
        let mut answer = Answer::text(status, "");
        answer.headers.push(("Location".into(), location));
        answer
    }

    /// The value of header `name`.
    fn header(&self, name: &str) -> Option<&str> {
        crate::header_in(&self.headers, name)
    }

    /// Sets header `name` to `value` unless already present.
    fn or_header(&mut self, name: &str, value: String) {
        if self.header(name).is_none() {
            self.headers.push((name.into(), value));
        }
    }
}

/// What the server does with a connection.
pub(crate) enum Outcome {
    /// Write this answer.
    Answer(Answer),
    /// Close without a byte.
    Close,
    /// Wait this long, then close.
    Hang(Duration),
    /// Write the head and only this many bytes of the body.
    Truncate(Answer, usize),
}

/// One request as read off the wire.
pub(crate) struct Request {
    /// The origin it reached.
    pub(crate) origin: FakeOrigin,
    /// The method.
    pub(crate) method: String,
    /// The request target, path and query.
    pub(crate) target: String,
    /// Every header, the credential included.
    pub(crate) headers: Vec<(String, String)>,
    /// The body as read (`Content-Length` of it, at most the server's bound; empty when
    /// chunked or after a malformed head).
    pub(crate) body: Vec<u8>,
    /// Whether the body was chunked.
    pub(crate) chunked: bool,
    /// Whether the request head was malformed.
    pub(crate) malformed: bool,
}

impl Request {
    /// The value of header `name`.
    fn header(&self, name: &str) -> Option<&str> {
        crate::header_in(&self.headers, name)
    }

    /// The path, without the query.
    fn path(&self) -> &str {
        self.target.split_once('?').map_or(&self.target, |(p, _)| p)
    }

    /// The query's pairs as sent.
    fn raw_query(&self) -> Vec<(&str, &str)> {
        self.target
            .split_once('?')
            .map(|(_, q)| q)
            .unwrap_or("")
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| p.split_once('=').unwrap_or((p, "")))
            .collect()
    }

    /// The decoded value of query parameter `name`.
    fn query(&self, name: &str) -> Option<String> {
        self.raw_query()
            .into_iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| aterm_codec::pct::decode(v))
    }

    /// Whether the method is a read.
    fn is_read(&self) -> bool {
        self.method == "GET" || self.method == "HEAD"
    }

    /// Whether the method is GET.
    fn is_get(&self) -> bool {
        self.method == "GET"
    }

    /// The credential's token.
    fn token(&self) -> Option<String> {
        let value = self.header(CREDENTIAL)?;
        let (scheme, token) = value.split_once(' ').unwrap_or((value, ""));
        let known = scheme == "Bearer" || scheme == "token";
        Some(if known {
            token.to_string()
        } else {
            String::new()
        })
    }
}

/// The credential header's name.
pub(crate) const CREDENTIAL: &str = "authorization";

/// Whether header `name: value` carries a credential at `origin`.
fn is_credential(state: &State, origin: FakeOrigin, name: &str, value: &str) -> bool {
    if name.eq_ignore_ascii_case(CREDENTIAL) {
        return true;
    }
    if !matches!(origin, FakeOrigin::Web | FakeOrigin::Cdn) {
        return false;
    }
    name.eq_ignore_ascii_case("proxy-authorization")
        || name.eq_ignore_ascii_case("cookie")
        || state
            .tokens
            .iter()
            .any(|(t, _)| !t.is_empty() && value.contains(t.as_str()))
}

/// The number of headers of `req` that carry a credential at its origin.
fn credentials(state: &State, req: &Request) -> usize {
    req.headers
        .iter()
        .filter(|(n, v)| is_credential(state, req.origin, n, v))
        .count()
}

/// Whether the request target (path or query) holds a token `add_token` registered.
fn target_credential(state: &State, req: &Request) -> bool {
    state
        .tokens
        .iter()
        .any(|(t, _)| !t.is_empty() && req.target.contains(t.as_str()))
}

/// Whether the request carries more than one credential header.
fn doubled_credential(req: &Request) -> bool {
    req.headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(CREDENTIAL))
        .count()
        > 1
}

/// What stands in for a registered token in a logged target or a violation text.
const TOKEN_SHOWN: &str = "<token>";

/// The request target with every registered token replaced by `TOKEN_SHOWN`, longest first.
fn shown_target(state: &State, req: &Request) -> String {
    let mut tokens: Vec<&str> = state
        .tokens
        .iter()
        .map(|(t, _)| t.as_str())
        .filter(|t| !t.is_empty())
        .collect();
    tokens.sort_by_key(|t| std::cmp::Reverse(t.len()));
    tokens
        .into_iter()
        .fold(req.target.clone(), |shown, t| shown.replace(t, TOKEN_SHOWN))
}

/// Whether header `name` is missing or has an empty value.
fn blank(req: &Request, name: &str) -> bool {
    req.header(name).is_none_or(|v| v.trim().is_empty())
}

/// The origin's name in violation texts.
const fn origin_name(origin: FakeOrigin) -> &'static str {
    match origin {
        FakeOrigin::Api => "api",
        FakeOrigin::Uploads => "uploads",
        FakeOrigin::Web => "web",
        FakeOrigin::Cdn => "cdn",
    }
}

/// Record every wire rule this request breaks.
fn violations(state: &mut State, req: &Request) {
    let m = &req.method;
    let path = shown_target(state, req);
    let foreign = matches!(req.origin, FakeOrigin::Web | FakeOrigin::Cdn);
    let o = origin_name(req.origin);
    let mut found = Vec::new();
    if req.malformed {
        state
            .violations
            .push(format!("malformed request head: {m} {path}"));
        return;
    }
    if foreign {
        for _ in 0..credentials(state, req) {
            found.push(format!(
                "credential header sent to the {o} origin: {m} {path}"
            ));
        }
    } else if doubled_credential(req) {
        found.push(format!(
            "more than one credential header at the {o} origin: {m} {path}"
        ));
    }
    if target_credential(state, req) {
        found.push(format!(
            "credential in the target sent to the {o} origin: {m} {path}"
        ));
    }
    if blank(req, "user-agent") {
        found.push(format!("no User-Agent: {m} {path}"));
    }
    if !foreign && blank(req, "x-github-api-version") {
        found.push(format!(
            "no X-GitHub-Api-Version at the {o} origin: {m} {path}"
        ));
    }
    if foreign {
        for (name, _) in &req.headers {
            if name.to_ascii_lowercase().starts_with("x-github-") {
                found.push(format!("{name} sent to the {o} origin: {m} {path}"));
            }
        }
    }
    if req.is_read() && req.header("content-length").is_some() {
        found.push(format!("Content-Length on a {m}: {path}"));
    }
    if let Some(n) = req.query("per_page").and_then(|v| v.parse::<u64>().ok())
        && n > 100
    {
        found.push(format!("per_page {n} over 100: {path}"));
    }
    state.violations.extend(found);
}

/// Log, judge and answer one request.
pub(crate) fn handle(state: &mut State, req: &Request) -> Outcome {
    state.requests += 1;
    let n = state.requests;
    let token = req.token();
    let login = token.as_deref().and_then(|t| state.login_of(t));
    let headers: Vec<(String, String)> = req
        .headers
        .iter()
        .filter(|(name, value)| !is_credential(state, req.origin, name, value))
        .cloned()
        .collect();
    let foreign = matches!(req.origin, FakeOrigin::Web | FakeOrigin::Cdn);
    let credentialed =
        headers.len() < req.headers.len() || (foreign && target_credential(state, req));
    state.seen.push(Seen {
        origin: req.origin,
        method: req.method.clone(),
        path_and_query: shown_target(state, req),
        headers,
        had_authorization: credentialed,
        as_login: login.clone(),
    });
    violations(state, req);
    let ctx = Ctx {
        token: login.as_ref().and(token),
        login,
        n,
    };
    let outcome = if req.malformed {
        Outcome::Answer(finish(
            state,
            req,
            &ctx,
            Answer::text(400, "malformed request head"),
        ))
    } else if req.chunked {
        Outcome::Answer(finish(
            state,
            req,
            &ctx,
            Answer::text(400, "chunked request"),
        ))
    } else if !foreign && doubled_credential(req) {
        Outcome::Answer(finish(
            state,
            req,
            &ctx,
            Answer::text(400, "more than one credential header"),
        ))
    } else if req.origin == FakeOrigin::Cdn && credentialed {
        Outcome::Answer(Answer::text(400, "credential sent to the CDN"))
    } else if let Some(fault) = state.take_fault(req.origin, &req.method, &req.target) {
        faulted(state, req, &ctx, fault)
    } else {
        Outcome::Answer(answer(state, req, &ctx))
    };
    if matches!(outcome, Outcome::Answer(_) | Outcome::Truncate(..)) {
        state.count_answered(req.origin, &req.method, &req.target);
    }
    outcome
}

/// Caller identity and request metadata.
struct Ctx {
    /// The token, when it is a known one.
    token: Option<String>,
    /// Its login.
    login: Option<String>,
    /// The request's number, from 1.
    n: u64,
}

impl Ctx {
    /// The login, borrowed.
    fn login(&self) -> Option<&str> {
        self.login.as_deref()
    }
}

/// What a fault does instead of the answer.
fn faulted(state: &mut State, req: &Request, ctx: &Ctx, fault: Fault) -> Outcome {
    match fault {
        Fault::Status {
            status,
            body,
            headers,
        } => {
            let mut a = Answer {
                status,
                headers,
                body: body.into_bytes(),
            };
            if a.header("content-type").is_none() {
                a.headers.insert(0, ("Content-Type".into(), JSON.into()));
            }
            Outcome::Answer(finish(state, req, ctx, a))
        }
        Fault::Close => {
            charge(state, req, ctx, None);
            Outcome::Close
        }
        Fault::Hang(d) => {
            charge(state, req, ctx, None);
            Outcome::Hang(d)
        }
        Fault::TruncateBody { at } => Outcome::Truncate(answer(state, req, ctx), at),
        Fault::SecondaryLimit { retry_after } => {
            let mut a = Answer::error(
                403,
                "You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
            );
            a.headers
                .push(("retry-after".into(), retry_after.to_string()));
            Outcome::Answer(finish(state, req, ctx, a))
        }
        Fault::PrimaryExhausted { reset } => {
            let a = Answer::error(403, "API rate limit exceeded.");
            Outcome::Answer(finish_with(state, req, ctx, a, Some(reset)))
        }
        Fault::RedirectTo { status, location } => {
            Outcome::Answer(finish(state, req, ctx, Answer::redirect(status, location)))
        }
        Fault::Torn => {
            let a = answer(state, req, ctx);
            if let Some(repo) = repo_of_path(state, req.path())
                && let Some(newest) = state.newest_release(state.repos[repo].id)
            {
                state.releases.retain(|r| r.id != newest);
            }
            Outcome::Answer(a)
        }
        Fault::SecondHop => {
            let cdn = state.ports.cdn;
            let a = Answer::redirect(302, format!("http://127.0.0.1:{cdn}{}", req.target));
            Outcome::Answer(finish(state, req, ctx, a))
        }
    }
}

/// The repository a path names, by `/repos/{o}/{r}` or `/repositories/{id}`.
fn repo_of_path(state: &State, path: &str) -> Option<usize> {
    match segments(path).as_slice() {
        ["repos", o, r, ..] => state.repo_index(&format!(
            "{}/{}",
            aterm_codec::pct::decode(o),
            aterm_codec::pct::decode(r)
        )),
        ["repositories", id, ..] => state.repo_by_id(id.parse().ok()?),
        _ => None,
    }
}

/// The answer the origin gives with no fault, its origin's rules applied.
fn answer(state: &mut State, req: &Request, ctx: &Ctx) -> Answer {
    match req.origin {
        FakeOrigin::Api | FakeOrigin::Uploads => {
            let a = if req.header(CREDENTIAL).is_some() && ctx.login.is_none() {
                Answer::error(401, "Bad credentials")
            } else {
                let a = if req.origin == FakeOrigin::Uploads {
                    upload(state, req, ctx)
                } else if req.is_read() {
                    etag(req, api(state, req, ctx))
                } else {
                    write(state, req, ctx)
                };
                if over_limit(state, req, ctx, a.status) {
                    let refused = Answer::error(403, "API rate limit exceeded.");
                    return finish_with(state, req, ctx, refused, Some(RESET));
                }
                a
            };
            finish(state, req, ctx, a)
        }
        FakeOrigin::Web => web(state, req),
        FakeOrigin::Cdn => cdn(state, req),
    }
}

/// Whether `req`, answered `status`, goes past its rate's limit.
fn over_limit(state: &State, req: &Request, ctx: &Ctx, status: u16) -> bool {
    if req.origin != FakeOrigin::Api || is_free(req, ctx, Some(status)) {
        return false;
    }
    let resource = resource_of(req);
    let limit = limit_for(ctx.token.is_some(), resource);
    let used = state
        .used
        .get(&(ctx.token.clone(), resource))
        .copied()
        .unwrap_or(0);
    used >= limit
}

/// An ETag on a JSON GET's 200, and a 304 to a matching `If-None-Match`.
fn etag(req: &Request, mut a: Answer) -> Answer {
    let json = a
        .header("content-type")
        .is_some_and(|t| t.starts_with("application/json"));
    if a.status != 200 || !json {
        return a;
    }
    let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&a.body));
    let tag = format!("W/\"{}\"", &digest[..16]);
    let hit = req.header("if-none-match").is_some_and(|v| {
        v.split(',')
            .map(str::trim)
            .any(|c| c == "*" || c == tag || c.trim_start_matches("W/") == &tag[2..])
    });
    if hit {
        return Answer {
            status: 304,
            headers: vec![("ETag".into(), tag)],
            body: Vec::new(),
        };
    }
    a.headers.push(("ETag".into(), tag));
    a
}

/// The api and uploads origins' headers: the request id, and on the api origin the rate.
fn finish(state: &mut State, req: &Request, ctx: &Ctx, a: Answer) -> Answer {
    finish_with(state, req, ctx, a, None)
}

/// As `finish`, with the rate exhausted until `exhausted` when given.
fn finish_with(
    state: &mut State,
    req: &Request,
    ctx: &Ctx,
    mut a: Answer,
    exhausted: Option<u64>,
) -> Answer {
    let api = req.origin == FakeOrigin::Api;
    if matches!(req.origin, FakeOrigin::Api | FakeOrigin::Uploads) {
        a.or_header("X-GitHub-Request-Id", format!("FAKE:{}", ctx.n));
    }
    if !api && exhausted.is_none() {
        return a;
    }
    let resource = resource_of(req);
    let limit = limit_for(ctx.token.is_some(), resource);
    let mut used = charge(state, req, ctx, Some(a.status)).unwrap_or(0);
    let mut reset = RESET;
    if let Some(at) = exhausted {
        used = limit;
        reset = at;
    }
    a.or_header("x-ratelimit-limit", limit.to_string());
    a.or_header("x-ratelimit-remaining", (limit - used).to_string());
    a.or_header("x-ratelimit-reset", reset.to_string());
    a.or_header("x-ratelimit-used", used.to_string());
    a.or_header("x-ratelimit-resource", resource.into());
    a
}

/// Rate-limit resource bucket for a request.
fn resource_of(req: &Request) -> &'static str {
    if segments(req.path()).first() == Some(&"search") {
        "search"
    } else {
        "core"
    }
}

/// Whether the rate leaves `req` uncharged.
fn is_free(req: &Request, ctx: &Ctx, status: Option<u16>) -> bool {
    segments(req.path()) == ["rate_limit"] || (ctx.token.is_some() && status == Some(304))
}

/// Use up one of the rate's requests for `req` on the api origin.
fn charge(state: &mut State, req: &Request, ctx: &Ctx, status: Option<u16>) -> Option<u64> {
    if req.origin != FakeOrigin::Api {
        return None;
    }
    let resource = resource_of(req);
    let limit = limit_for(ctx.token.is_some(), resource);
    let free = is_free(req, ctx, status);
    let count = state.used.entry((ctx.token.clone(), resource)).or_insert(0);
    if !free {
        *count = (*count + 1).min(limit);
    }
    Some(*count)
}

/// A resource's limit: 5000 per token and 60 anonymous (graphql: none anonymous).
fn limit_for(authenticated: bool, resource: &str) -> u64 {
    match (authenticated, resource) {
        (true, _) => 5000,
        (false, "graphql") => 0,
        (false, _) => 60,
    }
}

/// The path's segments, without the leading `/`.
fn segments(path: &str) -> Vec<&str> {
    path.trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

/// Segments decoded and joined with `/`.
fn joined(segs: &[&str]) -> String {
    segs.iter()
        .map(|s| aterm_codec::pct::decode(s))
        .collect::<Vec<_>>()
        .join("/")
}

/// `text` as one RFC 3986 path segment, as GitHub writes a tag or an asset name into a URL.
fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push_str(&aterm_codec::hex::encode_upper(&[b]));
        }
    }
    out
}

/// The api origin's routes.
fn api(state: &State, req: &Request, ctx: &Ctx) -> Answer {
    let path = req.path().to_string();
    let segs = segments(&path);
    match segs.as_slice() {
        ["rate_limit"] => rate_limit(state, ctx),
        ["user"] => match ctx.login() {
            Some(login) => user(state, login),
            None => Answer::error(401, "Requires authentication"),
        },
        ["users", login] => user(state, &aterm_codec::pct::decode(login)),
        ["user", "repos"] => user_repos(state, req, ctx),
        ["users", login, "repos"] => {
            let login = aterm_codec::pct::decode(login);
            match state.owner_named(&login).map(|o| o.id) {
                Some(id) => owner_repos(state, req, ctx, id, false),
                None => Answer::not_found(),
            }
        }
        ["user", id, "repos"] => match owner_by_id(state, id) {
            Some(id) => owner_repos(state, req, ctx, id, false),
            None => Answer::not_found(),
        },
        ["orgs", login, "repos"] => {
            let login = aterm_codec::pct::decode(login);
            match state.owner_named(&login).map(|o| o.id) {
                Some(id) => owner_repos(state, req, ctx, id, true),
                None => Answer::not_found(),
            }
        }
        ["organizations", id, "repos"] => match owner_by_id(state, id) {
            Some(id) => owner_repos(state, req, ctx, id, true),
            None => Answer::not_found(),
        },
        ["repos", o, r, rest @ ..] => {
            let slug = format!(
                "{}/{}",
                aterm_codec::pct::decode(o),
                aterm_codec::pct::decode(r)
            );
            match state.repo_index(&slug) {
                Some(i) => repo_route(state, req, ctx, i, rest),
                None => moved(state, req, ctx, &slug, rest),
            }
        }
        ["repositories", id, rest @ ..] => {
            match id.parse().ok().and_then(|id| state.repo_by_id(id)) {
                Some(i) => repo_route(state, req, ctx, i, rest),
                None => Answer::not_found(),
            }
        }
        _ => Answer::not_found(),
    }
}

/// The owner id a numeric segment names, when it is an owner's.
fn owner_by_id(state: &State, id: &str) -> Option<u64> {
    let id = id.parse::<u64>().ok()?;
    state.owners.iter().any(|o| o.id == id).then_some(id)
}

/// The 301 a renamed repository's old name answers, or a 404.
fn moved(state: &State, req: &Request, ctx: &Ctx, slug: &str, rest: &[&str]) -> Answer {
    let Some(id) = state
        .renames
        .iter()
        .rev()
        .find(|(old, _)| old.eq_ignore_ascii_case(slug))
        .map(|(_, id)| *id)
    else {
        return Answer::not_found();
    };
    let Some(i) = state.repo_by_id(id) else {
        return Answer::not_found();
    };
    if !state.repos[i].visible_to(ctx.login()) {
        return Answer::not_found();
    }
    let mut tail: String = rest.iter().map(|s| format!("/{s}")).collect();
    if let Some((_, query)) = req.target.split_once('?') {
        tail.push('?');
        tail.push_str(query);
    }
    let api = state.ports.api;
    let location = format!("http://127.0.0.1:{api}/repositories/{id}{tail}");
    let url = location.clone();
    let mut a = Answer::json(
        301,
        &json!({"message": "Moved Permanently", "url": url, "documentation_url": DOCS}),
    );
    a.headers.push(("Location".into(), location));
    a
}

/// An owner object.
fn owner_json(state: &State, login: &str) -> Value {
    let id = state.owner_named(login).map_or(0, |o| o.id);
    let kind = if state.is_user(login) {
        "User"
    } else {
        "Organization"
    };
    json!({"login": login, "id": id, "type": kind})
}

/// `/user` and `/users/{login}`.
fn user(state: &State, login: &str) -> Answer {
    let Some(owner) = state.owner_named(login) else {
        return Answer::not_found();
    };
    let web = state.ports.web;
    let mut value = owner_json(state, &owner.login);
    if let Some(map) = value.as_object_mut() {
        map.insert("name".into(), Value::Null);
        map.insert(
            "html_url".into(),
            format!("http://127.0.0.1:{web}/{}", owner.login).into(),
        );
    }
    Answer::json(200, &value)
}

/// A repository object; `permissions` only for a token's login.
fn repo_json(state: &State, repo: &Repo, login: Option<&str>) -> Value {
    let (api, web) = (state.ports.api, state.ports.web);
    let full = repo.full_name();
    let visibility = if repo.spec.private {
        "private"
    } else {
        "public"
    };
    let mut value = json!({
        "id": repo.id,
        "name": repo.name,
        "full_name": full,
        "owner": owner_json(state, &repo.owner),
        "private": repo.spec.private,
        "fork": false,
        "archived": false,
        "visibility": visibility,
        "default_branch": repo.spec.default_branch,
        "pushed_at": "2026-10-01T00:00:00Z",
        "description": null,
        "html_url": format!("http://127.0.0.1:{web}/{full}"),
        "url": format!("http://127.0.0.1:{api}/repos/{full}"),
    });
    if let (Some(login), Some(map)) = (login, value.as_object_mut()) {
        let push = repo.is_push(Some(login));
        map.insert(
            "permissions".into(),
            json!({"admin": false, "maintain": false, "push": push, "triage": push, "pull": true}),
        );
    }
    value
}

/// Computes the slice bounds for one page of `total` items.
struct Paging {
    /// Items per page.
    per_page: usize,
    /// This page's number, from 1.
    number: usize,
    /// The last page's number (1 for an empty list).
    last: usize,
}

impl Paging {
    /// The request's page of `total` items.
    fn of(req: &Request, total: usize) -> Paging {
        let per_page = req
            .query("per_page")
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .map_or(DEFAULT_PER_PAGE, |n| n.min(100));
        let number = req
            .query("page")
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(1);
        Paging {
            per_page,
            number,
            last: total.div_ceil(per_page).max(1),
        }
    }

    /// This page's items of `items`.
    fn take<T>(&self, items: impl IntoIterator<Item = T>) -> Vec<T> {
        items
            .into_iter()
            .skip((self.number - 1).saturating_mul(self.per_page))
            .take(self.per_page)
            .collect()
    }

    /// Formats the pagination `Link` header for `base`, or `None` for a single page.
    fn link(&self, req: &Request, base: &str) -> Option<String> {
        let (per_page, number, last) = (self.per_page, self.number, self.last);
        let kept: String = req
            .raw_query()
            .into_iter()
            .filter(|(k, _)| *k != "per_page" && *k != "page")
            .map(|(k, v)| format!("{k}={v}&"))
            .collect();
        let link = |n: usize, rel: &str| {
            format!("<{base}?{kept}per_page={per_page}&page={n}>; rel=\"{rel}\"")
        };
        let mut links = Vec::new();
        if number > 1 {
            links.push(link(number - 1, "prev"));
        }
        if number < last {
            links.push(link(number + 1, "next"));
            links.push(link(last, "last"));
        }
        if number > 1 {
            links.push(link(1, "first"));
        }
        (!links.is_empty()).then(|| links.join(", "))
    }
}

/// Returns the items for `page` and the optional pagination `Link` header.
fn page(req: &Request, items: Vec<Value>, base: &str) -> Answer {
    let paging = Paging::of(req, items.len());
    let mut a = Answer::json(200, &Value::Array(paging.take(items)));
    if let Some(link) = paging.link(req, base) {
        a.headers.push(("Link".into(), link));
    }
    a
}

/// `/rate_limit`: each resource as this caller stands.
fn rate_limit(state: &State, ctx: &Ctx) -> Answer {
    let entry = |resource: &'static str| {
        let limit = limit_for(ctx.token.is_some(), resource);
        let used = state
            .used
            .get(&(ctx.token.clone(), resource))
            .copied()
            .unwrap_or(0);
        json!({"limit": limit, "used": used, "remaining": limit - used, "reset": RESET})
    };
    let core = entry("core");
    Answer::json(
        200,
        &json!({
            "resources": {"core": core, "graphql": entry("graphql"), "search": entry("search")},
            "rate": core,
        }),
    )
}

/// `/user/repos`: the token's own repositories (`affiliation`), filtered by `visibility`.
fn user_repos(state: &State, req: &Request, ctx: &Ctx) -> Answer {
    let Some(login) = ctx.login() else {
        return Answer::error(401, "Requires authentication");
    };
    let affiliation = req
        .query("affiliation")
        .unwrap_or_else(|| "owner,collaborator,organization_member".into());
    let owner = affiliation.split(',').any(|a| a == "owner");
    let collaborator = affiliation.split(',').any(|a| a == "collaborator");
    let visibility = req.query("visibility").unwrap_or_else(|| "all".into());
    let items = state
        .repos
        .iter()
        .filter(|r| {
            (owner && r.owner.eq_ignore_ascii_case(login))
                || (collaborator && r.is_member(Some(login)))
        })
        .filter(|r| r.visible_to(Some(login)))
        .filter(|r| match visibility.as_str() {
            "public" => !r.spec.private,
            "private" => r.spec.private,
            _ => true,
        })
        .map(|r| repo_json(state, r, Some(login)))
        .collect();
    let api = state.ports.api;
    page(req, items, &format!("http://127.0.0.1:{api}/user/repos"))
}

/// Handles `/orgs/{o}/repos` and `/users/{u}/repos` with pagination links.
fn owner_repos(state: &State, req: &Request, ctx: &Ctx, id: u64, org: bool) -> Answer {
    let Some(owner) = state.owners.iter().find(|o| o.id == id) else {
        return Answer::not_found();
    };
    if org && state.is_user(&owner.login) {
        return Answer::not_found();
    }
    let kind = req.query("type").unwrap_or_else(|| "all".into());
    let items = state
        .repos
        .iter()
        .filter(|r| r.owner.eq_ignore_ascii_case(&owner.login))
        .filter(|r| {
            if org {
                r.visible_to(ctx.login())
            } else {
                !r.spec.private
            }
        })
        .filter(|r| match kind.as_str() {
            "public" => !r.spec.private,
            "private" => r.spec.private,
            "forks" => false,
            _ => true,
        })
        .map(|r| repo_json(state, r, ctx.login()))
        .collect();
    let api = state.ports.api;
    let base = if org {
        format!("http://127.0.0.1:{api}/organizations/{id}/repos")
    } else {
        format!("http://127.0.0.1:{api}/user/{id}/repos")
    };
    page(req, items, &base)
}

/// Every route under one repository.
fn repo_route(state: &State, req: &Request, ctx: &Ctx, index: usize, rest: &[&str]) -> Answer {
    if !state.repos[index].visible_to(ctx.login()) {
        return Answer::not_found();
    }
    let repo = &state.repos[index];
    match rest {
        [] => Answer::json(200, &repo_json(state, repo, ctx.login())),
        ["releases"] => releases(state, req, ctx, repo),
        ["releases", "latest"] => latest(state, repo),
        ["releases", "tags", tag @ ..] if !tag.is_empty() => {
            release_by_tag(state, repo, &joined(tag))
        }
        ["releases", "assets", id] => asset(state, req, ctx, repo, id),
        ["releases", id] => release_by_id(state, ctx, repo, id),
        ["contents", path @ ..] => contents(state, req, repo, &joined(path)),
        ["git", "trees", reference @ ..] if !reference.is_empty() => {
            tree(state, req, repo, &joined(reference))
        }
        ["commits"] => commits(state, req, repo),
        ["commits", reference @ ..] if !reference.is_empty() => {
            commit(state, repo, &joined(reference))
        }
        ["compare", range] => compare(state, req, repo, range),
        ["branches"] => branches(state, req, repo),
        ["tags"] => tags(state, req, repo),
        _ => Answer::not_found(),
    }
}

/// `/releases/tags/{tag}`: the newest published release of that tag.
fn release_by_tag(state: &State, repo: &Repo, tag: &str) -> Answer {
    let found = published(state, repo)
        .into_iter()
        .filter(|r| r.spec.tag == tag)
        .max_by(|a, b| (&a.spec.created_at, a.id).cmp(&(&b.spec.created_at, b.id)));
    found.map_or_else(Answer::not_found, |r| {
        Answer::json(200, &release_json(state, repo, r))
    })
}

/// `/releases/{id}`: the release of the repository, a draft only to a push login.
fn release_by_id(state: &State, ctx: &Ctx, repo: &Repo, id: &str) -> Answer {
    let found = id
        .parse::<u64>()
        .ok()
        .and_then(|id| {
            state
                .releases
                .iter()
                .find(|r| r.id == id && r.repo == repo.id)
        })
        .filter(|r| !r.spec.draft || repo.is_push(ctx.login()));
    found.map_or_else(Answer::not_found, |r| {
        Answer::json(200, &release_json(state, repo, r))
    })
}

/// `/commits/{ref}`: one commit, or 422 for a reference the history does not hold.
fn commit(state: &State, repo: &Repo, reference: &str) -> Answer {
    match repo.commit_index(reference) {
        Some(i) => Answer::json(200, &commit_json(state, repo, i)),
        None => Answer::error(422, &format!("No commit found for SHA: {reference}")),
    }
}

/// `/branches`: by name, each unprotected.
fn branches(state: &State, req: &Request, repo: &Repo) -> Answer {
    let refs = repo
        .branches
        .iter()
        .map(|(name, sha)| (name.as_str(), sha.as_str()));
    ref_page(
        state,
        req,
        repo,
        "branches",
        refs,
        |name, commit| json!({"name": name, "protected": false, "commit": commit}),
    )
}

/// `/tags`: newest set first.
fn tags(state: &State, req: &Request, repo: &Repo) -> Answer {
    let refs = repo
        .tags
        .iter()
        .rev()
        .map(|(name, sha)| (name.as_str(), sha.as_str()));
    ref_page(
        state,
        req,
        repo,
        "tags",
        refs,
        |name, commit| json!({"name": name, "commit": commit}),
    )
}

/// A page of ref entries.
fn ref_page<'r>(
    state: &State,
    req: &Request,
    repo: &Repo,
    kind: &str,
    refs: impl Iterator<Item = (&'r str, &'r str)>,
    entry: impl Fn(&str, Value) -> Value,
) -> Answer {
    let api = state.ports.api;
    let full = repo.full_name();
    let items = refs
        .map(|(name, sha)| {
            let commit = json!({"sha": sha,
                "url": format!("http://127.0.0.1:{api}/repos/{full}/commits/{sha}")});
            entry(name, commit)
        })
        .collect();
    page(
        req,
        items,
        &format!("http://127.0.0.1:{api}/repos/{full}/{kind}"),
    )
}

/// The repository's published releases.
fn published<'s>(state: &'s State, repo: &Repo) -> Vec<&'s Release> {
    state
        .releases
        .iter()
        .filter(|r| r.repo == repo.id && !r.spec.draft)
        .collect()
}

/// Handles `/releases` newest first; a page starting past item [`RELEASES_RESULT_CAP`] is 422.
fn releases(state: &State, req: &Request, ctx: &Ctx, repo: &Repo) -> Answer {
    let push = repo.is_push(ctx.login());
    let mut list: Vec<&Release> = state
        .releases
        .iter()
        .filter(|r| r.repo == repo.id && (push || !r.spec.draft))
        .collect();
    let paging = Paging::of(req, list.len());
    if (paging.number - 1).saturating_mul(paging.per_page) >= RELEASES_RESULT_CAP {
        return Answer::error(
            422,
            &format!("Only the first {RELEASES_RESULT_CAP} results are available."),
        );
    }
    list.sort_by(|a, b| (&b.spec.created_at, b.id).cmp(&(&a.spec.created_at, a.id)));
    let items = list
        .into_iter()
        .map(|r| release_json(state, repo, r))
        .collect();
    let api = state.ports.api;
    page(
        req,
        items,
        &format!("http://127.0.0.1:{api}/repositories/{}/releases", repo.id),
    )
}

/// `/releases/latest`: the `set_latest` release, else the newest published non-prerelease.
fn latest(state: &State, repo: &Repo) -> Answer {
    let set = match repo.latest {
        Latest::None => return Answer::not_found(),
        Latest::Set(id) => published(state, repo).into_iter().find(|r| r.id == id),
        Latest::Auto => None,
    };
    let found = set.or_else(|| {
        published(state, repo)
            .into_iter()
            .filter(|r| !r.spec.prerelease)
            .max_by(|a, b| (&a.spec.created_at, a.id).cmp(&(&b.spec.created_at, b.id)))
    });
    found.map_or_else(Answer::not_found, |r| {
        Answer::json(200, &release_json(state, repo, r))
    })
}

/// The CDN URL of asset `id`, with its signature.
fn cdn_url(state: &State, id: u64) -> String {
    let cdn = state.ports.cdn;
    format!("http://127.0.0.1:{cdn}/assets/{id}?sig={}", signature(id))
}

/// Eight hex digits that bind a CDN URL to its asset.
fn signature(id: u64) -> String {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(b"agh-fake/sig\0");
    hasher.update(id.to_string().as_bytes());
    let mut hex = aterm_codec::hex::encode_lower(&hasher.finalize());
    hex.truncate(8);
    hex
}

/// What an asset's `digest` field says.
fn digest_json(spec: &crate::AssetSpec) -> Value {
    let of = |bytes: &[u8]| {
        format!(
            "sha256:{}",
            aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
        )
    };
    match spec.digest {
        DigestSpec::True => of(&spec.bytes).into(),
        DigestSpec::Null => Value::Null,
        DigestSpec::Wrong => {
            let mut other = spec.bytes.clone();
            other.extend_from_slice(b"\0wrong");
            of(&other).into()
        }
    }
}

/// An asset object.
fn asset_json(state: &State, repo: &Repo, release: &Release, asset: &crate::state::Asset) -> Value {
    let (api, web) = (state.ports.api, state.ports.web);
    let full = repo.full_name();
    let (tag, name) = (&release.spec.tag, &asset.spec.name);
    json!({
        "id": asset.id,
        "name": name,
        "label": null,
        "size": asset.spec.bytes.len(),
        "digest": digest_json(&asset.spec),
        "state": asset.spec.state,
        "content_type": asset.spec.content_type,
        "created_at": asset.created_at,
        "updated_at": asset.spec.updated_at,
        "download_count": asset.download_count,
        "url": format!("http://127.0.0.1:{api}/repos/{full}/releases/assets/{}", asset.id),
        "browser_download_url": format!(
            "http://127.0.0.1:{web}/{full}/releases/download/{}/{}",
            segment(tag),
            segment(name)
        ),
    })
}

/// A release object.
fn release_json(state: &State, repo: &Repo, release: &Release) -> Value {
    let (api, web, uploads) = (state.ports.api, state.ports.web, state.ports.uploads);
    let full = repo.full_name();
    let spec = &release.spec;
    let id = release.id;
    let assets: Vec<Value> = release
        .assets
        .iter()
        .map(|a| asset_json(state, repo, release, a))
        .collect();
    json!({
        "id": id,
        "tag_name": spec.tag,
        "name": spec.name,
        "draft": spec.draft,
        "prerelease": spec.prerelease,
        "immutable": spec.immutable,
        "target_commitish": spec.target_commitish,
        "created_at": spec.created_at,
        "published_at": if spec.draft { None } else { spec.published_at.clone() },
        "body": spec.body,
        "html_url": format!("http://127.0.0.1:{web}/{full}/releases/tag/{}", segment(&spec.tag)),
        "url": format!("http://127.0.0.1:{api}/repos/{full}/releases/{id}"),
        "upload_url": format!("http://127.0.0.1:{uploads}/repos/{full}/releases/{id}/assets{{?name,label}}"),
        "assets": assets,
    })
}

/// `/releases/assets/{id}`.
fn asset(state: &State, req: &Request, ctx: &Ctx, repo: &Repo, id: &str) -> Answer {
    let Ok(id) = id.parse::<u64>() else {
        return Answer::not_found();
    };
    let push = repo.is_push(ctx.login());
    let found = state
        .releases
        .iter()
        .filter(|r| r.repo == repo.id && (push || !r.spec.draft))
        .find_map(|r| r.assets.iter().find(|a| a.id == id).map(|a| (r, a)));
    let Some((release, asset)) = found else {
        return Answer::not_found();
    };
    let bytes = req.header("accept").is_some_and(|a| a.contains(OCTET));
    if bytes {
        Answer::redirect(302, cdn_url(state, asset.id))
    } else {
        Answer::json(200, &asset_json(state, repo, release, asset))
    }
}

/// A blob's sha: forty hex digits bound to its bytes.
fn blob_sha(bytes: &[u8]) -> String {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(b"blob\0");
    hasher.update(bytes);
    let mut hex = aterm_codec::hex::encode_lower(&hasher.finalize());
    hex.truncate(40);
    hex
}

/// A directory's sha: forty hex digits bound to its path and reference.
fn dir_sha(reference: &str, path: &str) -> String {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(b"tree\0");
    hasher.update(reference.as_bytes());
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    let mut hex = aterm_codec::hex::encode_lower(&hasher.finalize());
    hex.truncate(40);
    hex
}

/// Encodes `bytes` as base64 wrapped every 60 characters with newlines.
fn wrapped_base64(bytes: &[u8]) -> String {
    let flat = aterm_codec::base64::encode(bytes).unwrap_or_default();
    let mut out = String::with_capacity(flat.len() + flat.len() / 60 + 1);
    for chunk in flat.as_bytes().chunks(60) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out
}

/// `/contents/{path}?ref=`: a file's bytes (Accept raw) or object, or a directory's array.
fn contents(state: &State, req: &Request, repo: &Repo, path: &str) -> Answer {
    let reference = req
        .query("ref")
        .unwrap_or_else(|| repo.spec.default_branch.clone());
    if !repo.knows(&reference) {
        return Answer::error(404, &format!("No commit found for the ref {reference}"));
    }
    let files = repo.files_at(&reference);
    let resolved = repo.resolve(&reference);
    let (api, web) = (state.ports.api, state.ports.web);
    let full = repo.full_name();
    let path = path.trim_matches('/');
    let raw = req.header("accept").is_some_and(|a| a.contains(".raw"));
    let encoded = |p: &str| p.split('/').map(segment).collect::<Vec<_>>().join("/");
    let (query_ref, path_ref) = (segment(&reference), encoded(&reference));
    let entry_url = |p: &str| {
        format!(
            "http://127.0.0.1:{api}/repos/{full}/contents/{}?ref={query_ref}",
            encoded(p)
        )
    };
    let download = |p: &str| {
        format!(
            "http://127.0.0.1:{web}/{full}/raw/{path_ref}/{}",
            encoded(p)
        )
    };
    if let Some(file) = files.iter().find(|f| f.path == path) {
        if raw {
            return Answer {
                status: 200,
                headers: vec![("Content-Type".into(), RAW_FILE.into())],
                body: file.bytes.clone(),
            };
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        return Answer::json(
            200,
            &json!({
                "type": "file",
                "name": name,
                "path": path,
                "size": file.bytes.len(),
                "sha": blob_sha(&file.bytes),
                "encoding": "base64",
                "content": wrapped_base64(&file.bytes),
                "url": entry_url(path),
                "download_url": download(path),
            }),
        );
    }
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}/")
    };
    let under: Vec<&&File> = files
        .iter()
        .filter(|f| f.path.starts_with(&prefix))
        .collect();
    if under.is_empty() && !path.is_empty() {
        return Answer::not_found();
    }
    let mut names: Vec<(String, Option<&File>)> = Vec::new();
    for file in under {
        let rest = &file.path[prefix.len()..];
        let (name, is_file) = match rest.split_once('/') {
            Some((dir, _)) => (dir.to_string(), false),
            None => (rest.to_string(), true),
        };
        if !names.iter().any(|(n, _)| *n == name) {
            names.push((name, is_file.then_some(*file)));
        }
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    let items: Vec<Value> = names
        .into_iter()
        .map(|(name, file)| {
            let p = format!("{prefix}{name}");
            match file {
                Some(f) => json!({"type": "file", "name": name, "path": p,
                    "size": f.bytes.len(), "sha": blob_sha(&f.bytes),
                    "url": entry_url(&p), "download_url": download(&p)}),
                None => json!({"type": "dir", "name": name, "path": p, "size": 0,
                    "sha": dir_sha(&resolved, &p), "url": entry_url(&p), "download_url": null}),
            }
        })
        .collect();
    Answer::json(200, &Value::Array(items))
}

/// `/git/trees/{ref}?recursive=…`: every path laid under the ref, and `truncated`.
fn tree(state: &State, req: &Request, repo: &Repo, reference: &str) -> Answer {
    if !repo.knows(reference) {
        return Answer::not_found();
    }
    let recursive = req.query("recursive").is_some();
    let api = state.ports.api;
    let full = repo.full_name();
    let resolved = repo.resolve(reference);
    let mut entries: Vec<(String, Option<&File>)> = Vec::new();
    for file in repo.files_at(reference) {
        let parts: Vec<&str> = file.path.split('/').collect();
        for depth in 1..parts.len() {
            let dir = parts[..depth].join("/");
            if !entries.iter().any(|(p, _)| *p == dir) {
                entries.push((dir, None));
            }
        }
        entries.push((file.path.clone(), Some(file)));
    }
    if !recursive {
        entries.retain(|(p, _)| !p.contains('/'));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let tree: Vec<Value> = entries
        .into_iter()
        .map(|(path, file)| match file {
            Some(f) => {
                let sha = blob_sha(&f.bytes);
                json!({"path": path, "mode": "100644", "type": "blob", "sha": sha,
                    "size": f.bytes.len(),
                    "url": format!("http://127.0.0.1:{api}/repos/{full}/git/blobs/{sha}")})
            }
            None => {
                let sha = dir_sha(&resolved, &path);
                json!({"path": path, "mode": "040000", "type": "tree", "sha": sha,
                    "url": format!("http://127.0.0.1:{api}/repos/{full}/git/trees/{sha}")})
            }
        })
        .collect();
    let sha = dir_sha(&resolved, "");
    Answer::json(
        200,
        &json!({"sha": sha, "url": format!("http://127.0.0.1:{api}/repos/{full}/git/trees/{sha}"),
            "tree": tree, "truncated": repo.tree_truncated}),
    )
}

/// A commit object.
fn commit_json(state: &State, repo: &Repo, index: usize) -> Value {
    let (api, web) = (state.ports.api, state.ports.web);
    let full = repo.full_name();
    let c = &repo.commits[index];
    let actor = json!({"name": c.author_name, "email": "noreply", "date": c.date});
    let author = c
        .author_login
        .as_ref()
        .map_or(Value::Null, |l| json!({"login": l}));
    json!({
        "sha": c.sha,
        "commit": {"message": c.message, "author": actor, "committer": actor},
        "author": author,
        "files": c.paths.iter().map(|p| json!({"filename": p, "status": "modified"})).collect::<Vec<_>>(),
        "html_url": format!("http://127.0.0.1:{web}/{full}/commit/{}", c.sha),
        "url": format!("http://127.0.0.1:{api}/repos/{full}/commits/{}", c.sha),
    })
}

/// `/commits?sha=&path=&since=&author=`: newest first from the ref.
fn commits(state: &State, req: &Request, repo: &Repo) -> Answer {
    let reference = req
        .query("sha")
        .unwrap_or_else(|| repo.spec.default_branch.clone());
    let Some(top) = repo.commit_index(&reference) else {
        return Answer::not_found();
    };
    let path = req.query("path").map(|p| p.trim_matches('/').to_string());
    let since = req.query("since");
    let author = req.query("author");
    let items = (0..=top)
        .rev()
        .filter(|&i| {
            let c = &repo.commits[i];
            let on_path = path.as_ref().is_none_or(|p| {
                c.paths
                    .iter()
                    .any(|q| q == p || q.starts_with(&format!("{p}/")))
            });
            let after = since.as_ref().is_none_or(|s| c.date.as_str() >= s.as_str());
            let by = author.as_ref().is_none_or(|a| {
                c.author_login
                    .as_ref()
                    .is_some_and(|l| l.eq_ignore_ascii_case(a))
                    || c.author_name.eq_ignore_ascii_case(a)
            });
            on_path && after && by
        })
        .map(|i| commit_json(state, repo, i))
        .collect();
    let api = state.ports.api;
    page(
        req,
        items,
        &format!("http://127.0.0.1:{api}/repositories/{}/commits", repo.id),
    )
}

/// GitHub's cap on an unpaged comparison's commits.
const COMPARE_COMMITS: usize = 250;
/// GitHub's cap on a comparison's files.
const COMPARE_FILES: usize = 300;

/// `/compare/{base}...{head}`.
fn compare(state: &State, req: &Request, repo: &Repo, raw_range: &str) -> Answer {
    let range = aterm_codec::pct::decode(raw_range);
    let Some((base, head)) = range.split_once("...") else {
        return Answer::not_found();
    };
    let Some((_, _, spec)) = repo
        .compares
        .iter()
        .find(|(b, h, _)| b == base && h == head)
    else {
        return Answer::not_found();
    };
    let CompareSpec::Ok {
        status,
        ahead_by,
        behind_by,
        total_commits,
        merge_base,
    } = spec
    else {
        return Answer::error(
            404,
            &format!("No common ancestor between {base} and {head}."),
        );
    };
    let all: Vec<usize> = match (repo.commit_index(merge_base), repo.commit_index(head)) {
        (Some(from), Some(to)) if from < to => (from + 1..=to).collect(),
        _ => Vec::new(),
    };
    let unpaged = req.query("page").is_none() && req.query("per_page").is_none();
    let (listed, link, first) = if unpaged {
        let tail = all[all.len().saturating_sub(COMPARE_COMMITS)..].to_vec();
        (tail, None, true)
    } else {
        let paging = Paging::of(req, all.len());
        let api = state.ports.api;
        let full = repo.full_name();
        let base_url = format!("http://127.0.0.1:{api}/repos/{full}/compare/{raw_range}");
        let link = paging.link(req, &base_url);
        (paging.take(all.iter().copied()), link, paging.number == 1)
    };
    let mut files: Vec<String> = Vec::new();
    if first {
        for &i in &all {
            for p in &repo.commits[i].paths {
                if files.len() < COMPARE_FILES && !files.contains(p) {
                    files.push(p.clone());
                }
            }
        }
    }
    let commits: Vec<Value> = listed
        .iter()
        .map(|&i| commit_json(state, repo, i))
        .collect();
    let mut a = Answer::json(
        200,
        &json!({
            "status": status,
            "ahead_by": ahead_by,
            "behind_by": behind_by,
            "total_commits": total_commits,
            "merge_base_commit": {"sha": merge_base},
            "base_commit": {"sha": repo.resolve(base)},
            "commits": commits,
            "files": files.iter().map(|f| json!({"filename": f, "status": "modified"})).collect::<Vec<_>>(),
        }),
    );
    if let Some(link) = link {
        a.headers.push(("Link".into(), link));
    }
    a
}

/// The web origin.
fn web(state: &State, req: &Request) -> Answer {
    let path = req.path().to_string();
    let segs = segments(&path);
    let ["releases", "download", tag @ .., name] = segs.get(2..).unwrap_or(&[]) else {
        return Answer::text(404, "Not Found");
    };
    if tag.is_empty() || !req.is_read() {
        return Answer::text(404, "Not Found");
    }
    let slug = format!("{}/{}", segs[0], segs[1]);
    let (tag, name) = (joined(tag), aterm_codec::pct::decode(name));
    let found = state
        .repo_index(&slug)
        .map(|i| &state.repos[i])
        .filter(|r| !r.spec.private)
        .and_then(|r| {
            published(state, r)
                .into_iter()
                .filter(|rel| rel.spec.tag == tag)
                .find_map(|rel| rel.assets.iter().find(|a| a.spec.name == name))
        });
    match found {
        Some(asset) => Answer::redirect(302, cdn_url(state, asset.id)),
        None => Answer::text(404, "Not Found"),
    }
}

/// The CDN: `/assets/{id}?sig=…` → the bytes; each GET counts a download.
fn cdn(state: &mut State, req: &Request) -> Answer {
    let path = req.path().to_string();
    let segs = segments(&path);
    let id = match segs.as_slice() {
        ["assets", id] if req.is_read() => id.parse::<u64>().ok(),
        _ => None,
    };
    let Some(id) = id else {
        return Answer::text(404, "Not Found");
    };
    if req.query("sig").as_deref() != Some(signature(id).as_str()) {
        return Answer::text(403, "Forbidden");
    }
    let is_get = req.is_get();
    let found = state
        .releases
        .iter_mut()
        .find_map(|r| r.assets.iter_mut().find(|a| a.id == id));
    match found {
        Some(asset) => {
            if is_get {
                asset.download_count += 1;
            }
            Answer {
                status: 200,
                headers: vec![("Content-Type".into(), OCTET.into())],
                body: asset.spec.bytes.clone(),
            }
        }
        None => Answer::text(404, "Not Found"),
    }
}

/// GitHub's 422 for one field: `Validation Failed` and the one error naming it.
fn invalid(resource: &str, field: &str, code: &str) -> Answer {
    Answer::json(
        422,
        &json!({
            "message": "Validation Failed",
            "errors": [{"resource": resource, "code": code, "field": field}],
            "documentation_url": DOCS,
            "status": "422",
        }),
    )
}

/// Why `login` may not write to `repo`, if it may not: anonymous is 401, a repository it
/// cannot see is the 404 everything hidden answers, and one it sees without push is 403.
fn unwritable(repo: &Repo, ctx: &Ctx) -> Option<Answer> {
    if ctx.login().is_none() {
        Some(Answer::error(401, "Requires authentication"))
    } else if !repo.visible_to(ctx.login()) {
        Some(Answer::not_found())
    } else if !repo.is_push(ctx.login()) {
        Some(Answer::error(
            403,
            "Resource not accessible by personal access token",
        ))
    } else {
        None
    }
}

/// A boolean field of a JSON request: absent is `None`; a value of another type is `Err`.
fn flag(body: &Value, name: &str) -> Result<Option<bool>, ()> {
    match body.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(()),
    }
}

/// A string field of a JSON request: absent is `None`; a value of another type is `Err`.
fn text_field(body: &Value, name: &str) -> Result<Option<String>, ()> {
    match body.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(()),
    }
}

/// The api origin's writes: create (`POST /repos/{o}/{r}/releases`), edit (`PATCH
/// …/releases/{id}`) and delete (`DELETE …/releases/{id}`) a release, and rename (`PATCH
/// …/releases/assets/{id}`) and delete (`DELETE …/releases/assets/{id}`) an asset.
fn write(state: &mut State, req: &Request, ctx: &Ctx) -> Answer {
    let path = req.path().to_string();
    let segs = segments(&path);
    let ["repos", o, r, rest @ ..] = segs.as_slice() else {
        return Answer::not_found();
    };
    let slug = format!(
        "{}/{}",
        aterm_codec::pct::decode(o),
        aterm_codec::pct::decode(r)
    );
    let Some(index) = state.repo_index(&slug) else {
        return Answer::not_found();
    };
    if let Some(refused) = unwritable(&state.repos[index], ctx) {
        return refused;
    }
    match (req.method.as_str(), rest) {
        ("POST", ["releases"]) => create_release(state, req, index),
        ("PATCH", ["releases", id]) => match release_of(state, index, id) {
            Some(at) => edit_release(state, req, index, at),
            None => Answer::not_found(),
        },
        ("PATCH", ["releases", "assets", id]) => edit_asset(state, req, index, id),
        ("DELETE", ["releases", "assets", id]) => delete_asset(state, index, id),
        ("DELETE", ["releases", id]) => match release_of(state, index, id) {
            Some(at) => {
                state.releases.remove(at);
                Answer {
                    status: 204,
                    headers: Vec::new(),
                    body: Vec::new(),
                }
            }
            None => Answer::not_found(),
        },
        _ => Answer::not_found(),
    }
}

/// The index in `state.releases` of release `id` of repository `index`.
fn release_of(state: &State, index: usize, id: &str) -> Option<usize> {
    let repo = state.repos[index].id;
    let id = id.parse::<u64>().ok()?;
    state
        .releases
        .iter()
        .position(|r| r.id == id && r.repo == repo)
}

/// Whether repository `repo` holds a published release of `tag` other than `except`.
fn published_tag(state: &State, repo: u64, tag: &str, except: Option<u64>) -> bool {
    state
        .releases
        .iter()
        .any(|r| r.repo == repo && r.spec.tag == tag && !r.spec.draft && Some(r.id) != except)
}

/// What `make_latest` does once release `id` of repository `index` is written: `"true"` makes
/// a published non-prerelease the latest; `"false"` keeps the latest where it is (an
/// automatic one is pinned to what it names now, so this release cannot take it).
fn apply_make_latest(state: &mut State, index: usize, id: u64, make_latest: Option<&str>) {
    let repo = &state.repos[index];
    let is_candidate = state
        .releases
        .iter()
        .find(|r| r.id == id)
        .is_some_and(|r| !r.spec.draft && !r.spec.prerelease);
    match make_latest {
        Some("true") if is_candidate => state.repos[index].latest = Latest::Set(id),
        Some("false") if repo.latest == Latest::Auto => {
            let current = published(state, repo)
                .into_iter()
                .filter(|r| r.id != id && !r.spec.prerelease)
                .max_by(|a, b| (&a.spec.created_at, a.id).cmp(&(&b.spec.created_at, b.id)))
                .map(|r| r.id);
            state.repos[index].latest = current.map_or(Latest::None, Latest::Set);
        }
        _ => {}
    }
}

/// `POST /repos/{o}/{r}/releases`: a release of `tag_name` (a draft or published, a
/// prerelease or not).
fn create_release(state: &mut State, req: &Request, index: usize) -> Answer {
    let Ok(body) = aterm_json::from_slice::<Value>(&req.body) else {
        return Answer::error(400, "Problems parsing JSON");
    };
    let (Ok(tag), Ok(name), Ok(notes), Ok(target), Ok(make_latest)) = (
        text_field(&body, "tag_name"),
        text_field(&body, "name"),
        text_field(&body, "body"),
        text_field(&body, "target_commitish"),
        text_field(&body, "make_latest"),
    ) else {
        return Answer::error(400, "Problems parsing JSON");
    };
    let (Ok(draft), Ok(prerelease)) = (flag(&body, "draft"), flag(&body, "prerelease")) else {
        return Answer::error(400, "Problems parsing JSON");
    };
    let Some(tag) = tag.filter(|t| !t.is_empty()) else {
        return invalid("Release", "tag_name", "missing_field");
    };
    let repo = &state.repos[index];
    let repo_id = repo.id;
    if published_tag(state, repo_id, &tag, None) {
        return invalid("Release", "tag_name", "already_exists");
    }
    let draft = draft.unwrap_or(false);
    let created_at = "2026-11-01T00:00:00Z".to_string();
    let spec = ReleaseSpec {
        tag,
        name,
        draft,
        prerelease: prerelease.unwrap_or(false),
        immutable: false,
        target_commitish: target.unwrap_or_else(|| repo.spec.default_branch.clone()),
        published_at: (!draft).then(|| created_at.clone()),
        created_at,
        body: notes,
    };
    let full = repo.full_name();
    let id = state.add_release(&full, spec);
    apply_make_latest(state, index, id, make_latest.as_deref());
    let repo = &state.repos[index];
    match state.releases.iter().find(|r| r.id == id) {
        Some(release) => Answer::json(201, &release_json(state, repo, release)),
        None => Answer::not_found(),
    }
}

/// `PATCH …/releases/{id}`: the fields the body names.
fn edit_release(state: &mut State, req: &Request, index: usize, at: usize) -> Answer {
    let Ok(body) = aterm_json::from_slice::<Value>(&req.body) else {
        return Answer::error(400, "Problems parsing JSON");
    };
    let (Ok(tag), Ok(name), Ok(notes), Ok(make_latest)) = (
        text_field(&body, "tag_name"),
        text_field(&body, "name"),
        text_field(&body, "body"),
        text_field(&body, "make_latest"),
    ) else {
        return Answer::error(400, "Problems parsing JSON");
    };
    let (Ok(draft), Ok(prerelease)) = (flag(&body, "draft"), flag(&body, "prerelease")) else {
        return Answer::error(400, "Problems parsing JSON");
    };
    let id = state.releases[at].id;
    let repo_id = state.repos[index].id;
    let final_tag = tag
        .clone()
        .unwrap_or_else(|| state.releases[at].spec.tag.clone());
    let final_draft = draft.unwrap_or(state.releases[at].spec.draft);
    if !final_draft && published_tag(state, repo_id, &final_tag, Some(id)) {
        return invalid("Release", "tag_name", "already_exists");
    }
    let spec = &mut state.releases[at].spec;
    if let Some(tag) = tag {
        spec.tag = tag;
    }
    if name.is_some() {
        spec.name = name;
    }
    if notes.is_some() {
        spec.body = notes;
    }
    if let Some(prerelease) = prerelease {
        spec.prerelease = prerelease;
    }
    if let Some(draft) = draft {
        spec.draft = draft;
        spec.published_at = if draft {
            None
        } else {
            spec.published_at
                .clone()
                .or_else(|| Some(spec.created_at.clone()))
        };
    }
    apply_make_latest(state, index, id, make_latest.as_deref());
    let repo = &state.repos[index];
    Answer::json(200, &release_json(state, repo, &state.releases[at]))
}

/// `PATCH …/releases/assets/{id}`: the asset's `name`, as the body names it, on whichever
/// release of the repository holds it.
fn edit_asset(state: &mut State, req: &Request, index: usize, id: &str) -> Answer {
    let Ok(body) = aterm_json::from_slice::<Value>(&req.body) else {
        return Answer::error(400, "Problems parsing JSON");
    };
    let Ok(name) = text_field(&body, "name") else {
        return Answer::error(400, "Problems parsing JSON");
    };
    let repo = state.repos[index].id;
    let Ok(id) = id.parse::<u64>() else {
        return Answer::not_found();
    };
    let Some(at) = state
        .releases
        .iter()
        .position(|r| r.repo == repo && r.assets.iter().any(|a| a.id == id))
    else {
        return Answer::not_found();
    };
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        let release = &mut state.releases[at];
        if release
            .assets
            .iter()
            .any(|a| a.id != id && a.spec.name == name)
        {
            return invalid("ReleaseAsset", "name", "already_exists");
        }
        if let Some(asset) = release.assets.iter_mut().find(|a| a.id == id) {
            asset.spec.name = name;
        }
    }
    let repo = &state.repos[index];
    let release = &state.releases[at];
    match release.assets.iter().find(|a| a.id == id) {
        Some(asset) => Answer::json(200, &asset_json(state, repo, release, asset)),
        None => Answer::not_found(),
    }
}

/// `DELETE …/releases/assets/{id}`: the asset, from whichever release of the repository holds
/// it.
fn delete_asset(state: &mut State, index: usize, id: &str) -> Answer {
    let repo = state.repos[index].id;
    let Ok(id) = id.parse::<u64>() else {
        return Answer::not_found();
    };
    for release in state.releases.iter_mut().filter(|r| r.repo == repo) {
        if let Some(at) = release.assets.iter().position(|a| a.id == id) {
            release.assets.remove(at);
            return Answer {
                status: 204,
                headers: Vec::new(),
                body: Vec::new(),
            };
        }
    }
    Answer::not_found()
}

/// The uploads origin: `POST /repos/{o}/{r}/releases/{id}/assets?name=…`, the request's body
/// as a new asset of that name under the request's `Content-Type`.
fn upload(state: &mut State, req: &Request, ctx: &Ctx) -> Answer {
    let path = req.path().to_string();
    let segs = segments(&path);
    let ["repos", o, r, "releases", id, "assets"] = segs.as_slice() else {
        return Answer::not_found();
    };
    if req.method != "POST" {
        return Answer::not_found();
    }
    let slug = format!(
        "{}/{}",
        aterm_codec::pct::decode(o),
        aterm_codec::pct::decode(r)
    );
    let Some(index) = state.repo_index(&slug) else {
        return Answer::not_found();
    };
    if let Some(refused) = unwritable(&state.repos[index], ctx) {
        return refused;
    }
    let Some(at) = release_of(state, index, id) else {
        return Answer::not_found();
    };
    let Some(name) = req.query("name").filter(|n| !n.is_empty()) else {
        return invalid("ReleaseAsset", "name", "missing_field");
    };
    let Some(content_type) = req.header("content-type").filter(|t| !t.is_empty()) else {
        return invalid("ReleaseAsset", "content_type", "missing_field");
    };
    if state.releases[at]
        .assets
        .iter()
        .any(|a| a.spec.name == name)
    {
        return invalid("ReleaseAsset", "name", "already_exists");
    }
    let release_id = state.releases[at].id;
    let spec = crate::AssetSpec {
        content_type: content_type.to_string(),
        ..crate::AssetSpec::uploaded(&name, &req.body)
    };
    let asset_id = state.add_asset(release_id, spec);
    let repo = &state.repos[index];
    let release = &state.releases[at];
    match release.assets.iter().find(|a| a.id == asset_id) {
        Some(asset) => Answer::json(201, &asset_json(state, repo, release, asset)),
        None => Answer::not_found(),
    }
}
