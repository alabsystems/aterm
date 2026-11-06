// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Canonical tape encoding and decoding for every door a tape keeps: GitHub HTTP requests,
//! responses and wire failures, route decisions, facts, git runs and their answers, streamed
//! bodies, and credentialed requests with their write bodies.

use std::fmt::Write as _;
use std::io::{Cursor, Write as _};

#[cfg(any(unix, test))]
use crate::error::SideEffect;
use crate::error::{Error, Replay, Result};
use crate::git::GitOp;
use crate::policy::{
    AnonymousWhy, ChainStep, Decision, Named, Presence, Refusal, RefusalCode, RouteRequest,
    RouteTarget, Tier,
};
use crate::redact::Redactor;
use crate::wire::{
    Endpoints, Origin, OriginRole, Phase, WireBody, WireError, WireKind, WireRequest, WireResponse,
    door,
};

/// Lowercase sorted response headers retained across live, record, and replay modes.
pub const KEPT_HEADERS: [&str; 15] = [
    "content-type",
    "etag",
    "link",
    "location",
    "retry-after",
    "x-accepted-github-permissions",
    "x-accepted-oauth-scopes",
    "x-github-request-id",
    "x-github-sso",
    "x-oauth-scopes",
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "x-ratelimit-resource",
    "x-ratelimit-used",
];

/// A reply body over this many bytes is taped as its digest and size only.
pub const BODY_TAPE_MAX: usize = 1 << 20;

/// The query a redirect off the API is written with: a CDN's signature, or another host's token, is a bearer secret with an expiry.
const SIGNED_REDACTED: &str = "signed=redacted";

/// The line a reply over `BODY_TAPE_MAX` holds in place of its body.
const BODY_DIGEST: &str = "body-digest";

/// `fn $name(slug) -> Option<$ty>`: the variant of `$ty` whose `slug()` is `slug`, among the
/// variants listed. The list is both the candidates and an exhaustive match over them (inside
/// agh, `#[non_exhaustive]` excuses no match), so a variant `$ty` gains and the list lacks
/// fails to compile here, never to decode on a replay.
macro_rules! by_slug {
    ($name:ident, $ty:ident: $($variant:ident),+ $(,)?) => {
        fn $name(slug: &str) -> Option<$ty> {
            [$($ty::$variant),+].into_iter().find(|candidate| match candidate {
                $($ty::$variant)|+ => candidate.slug() == slug,
            })
        }
    };
    // The same, with `$all` every listed variant, for a test that walks them.
    ($name:ident, $all:ident, $ty:ident: $($variant:ident),+ $(,)?) => {
        $crate::fx::canon::by_slug!($name, $ty: $($variant),+);
        #[cfg(test)]
        const $all: &[$ty] = &[$($ty::$variant),+];
    };
}
pub(crate) use by_slug;

// How `decode` reads a `failed` line's wire kind back.
by_slug!(
    wire_kind,
    WireKind: Dns,
    Connect,
    Proxy,
    Tls,
    Trust,
    Timeout,
    Idle,
    Reset,
    Truncated,
    TooLarge,
    Protocol,
    Revoked,
    Local,
);

/// Whether `name` is kept, ASCII case-insensitively.
#[must_use]
pub fn kept(name: &str) -> bool {
    KEPT_HEADERS.iter().any(|k| k.eq_ignore_ascii_case(name))
}

/// Filters `headers` to [`KEPT_HEADERS`] in received order with lowercased names and unmodified values.
pub(super) fn cut(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .filter(|(name, _)| kept(name))
        .map(|(mut name, value)| {
            name.make_ascii_lowercase();
            (name, value)
        })
        .collect()
}

/// Filters `headers` to [`KEPT_HEADERS`] with lowercased names, redacting the query of a `location` URL whose origin is an asset host or any origin but the API's (`signed_redacted`).
#[must_use]
pub fn keep(endpoints: &Endpoints, headers: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out = cut(headers);
    for (name, value) in &mut out {
        if name == "location"
            && let Some(redacted) = signed_redacted(endpoints, value)
        {
            *value = redacted;
        }
    }
    out
}

/// Replaces `url`'s query with `signed=redacted` when its origin is an asset host, or any
/// origin but the API's: a redirect off the API (an asset's CDN, codeload's `?token=`, an
/// artifact's or a log's signed blob URL) carries its bearer secret in its query. agh follows
/// a `location` only to the CDN (whose request is taped by its path) or on the API origin
/// (a moved repository), and those it reads whole.
fn signed_redacted(endpoints: &Endpoints, url: &str) -> Option<String> {
    let (origin, path_and_query) = Origin::parse(url)?;
    let api = endpoints.origin(OriginRole::Api);
    if origin == *api && !endpoints.asset_hosts().contains(&origin) {
        return None;
    }
    let (path, _query) = path_and_query.split_once('?')?;
    Some(format!("{}{path}?{SIGNED_REDACTED}", origin.base()))
}

/// The role's word on the `origin` line.
const fn role_word(role: OriginRole) -> &'static str {
    match role {
        OriginRole::Api => "api",
        OriginRole::Uploads => "uploads",
        OriginRole::Web => "web",
        OriginRole::AssetCdn => "cdn",
    }
}

/// Sorts `query`'s `&`-separated parameters by raw name and value bytes.
fn sorted_query(query: &str) -> std::borrow::Cow<'_, str> {
    if !query.contains('&') {
        return std::borrow::Cow::Borrowed(query);
    }
    let mut pairs: Vec<(&str, Option<&str>, &str)> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) => (name, Some(value), pair),
            None => (pair, None, pair),
        })
        .collect();
    pairs.sort_by(|a, b| {
        a.0.as_bytes()
            .cmp(b.0.as_bytes())
            .then_with(|| a.1.map(str::as_bytes).cmp(&b.1.map(str::as_bytes)))
    });
    let mut out = String::with_capacity(query.len());
    for (idx, (_, _, pair)) in pairs.iter().enumerate() {
        if idx > 0 {
            out.push('&');
        }
        out.push_str(pair);
    }
    std::borrow::Cow::Owned(out)
}

/// The next of a request an anonymous tape cannot hold: every run of it would be refused the same.
const ANONYMOUS_READS: &str =
    "run it without --record or --replay: an anonymous tape holds anonymous reads alone";

/// Encodes `req` into canonical tape request bytes (method, path and sorted or redacted query, origin role, sorted headers excluding `user-agent`, `as anonymous`, and `body none`).
///
/// # Errors
///
/// `Replay::StreamNotTaped` for a body other than `WireBody::Empty`, or a request with `auth`
/// set (an anonymous tape holds no credential); CouldNotRun, `Sub::Protocol`, for a URL on none of
/// `endpoints`' origins.
pub fn request(req: &WireRequest<'_>, endpoints: &Endpoints) -> Result<Vec<u8>> {
    if !matches!(req.body, WireBody::Empty) {
        return Err(Error::replay(
            Replay::StreamNotTaped,
            format!(
                "a {} with a body is not taped: an anonymous tape holds reads alone",
                req.method.as_str()
            ),
        )
        .with_next(ANONYMOUS_READS));
    }
    let credentialed = req.auth.is_some()
        || req
            .headers
            .iter()
            .any(|(name, _)| door::is_authorization(name));
    if credentialed {
        return Err(Error::replay(
            Replay::StreamNotTaped,
            "a request that carries a credential is not taped: this tape is anonymous",
        )
        .with_next(ANONYMOUS_READS));
    }
    let mut text = head(req, endpoints)?;
    text.push_str("\nas anonymous\nbody none");
    Ok(text.into_bytes())
}

/// The first lines of a request's canonical bytes, which `request` and `request_credentialed`
/// share: the method and the path with its sorted (or, on an asset host, redacted) query, the
/// `origin` line, and the sorted headers without `user-agent` or a credential header.
fn head(req: &WireRequest<'_>, endpoints: &Endpoints) -> Result<String> {
    let on_no_origin = || {
        Error::protocol(format!(
            "{} is on none of this client's origins",
            Redactor::shapes(&req.url)
        ))
    };
    let (origin, path_and_query) = Origin::parse(&req.url).ok_or_else(on_no_origin)?;
    let role = endpoints.role_of(&origin).ok_or_else(on_no_origin)?;
    let mut text = format!("{} ", req.method.as_str());
    match path_and_query.split_once('?') {
        Some((path, query)) => {
            text.push_str(path);
            text.push('?');
            if role == OriginRole::AssetCdn {
                text.push_str(SIGNED_REDACTED);
            } else {
                text.push_str(&sorted_query(query));
            }
        }
        None => text.push_str(&path_and_query),
    }
    text.push_str("\norigin ");
    text.push_str(role_word(role));
    let mut headers: Vec<(std::borrow::Cow<'_, str>, &str)> = req
        .headers
        .iter()
        .filter(|(name, _)| {
            !name.eq_ignore_ascii_case("user-agent") && !door::is_authorization(name)
        })
        .map(|(name, value)| {
            let lower = if name.bytes().any(|b| b.is_ascii_uppercase()) {
                std::borrow::Cow::Owned(name.to_ascii_lowercase())
            } else {
                std::borrow::Cow::Borrowed(name.as_str())
            };
            (lower, value.as_str())
        })
        .collect();
    headers.sort_by(|a, b| {
        a.0.as_bytes()
            .cmp(b.0.as_bytes())
            .then_with(|| a.1.as_bytes().cmp(b.1.as_bytes()))
    });
    for (name, value) in headers {
        text.push('\n');
        text.push_str(&name);
        text.push(' ');
        text.push_str(value);
    }
    Ok(text)
}

/// `sha256:{hex}` of `bytes`.
fn sha256_of(bytes: &[u8]) -> String {
    crate::model::Sha256Digest::of(bytes).to_string()
}

/// `sha256:{hex} {len}` of `canonical`: the journal's `request` field (§4.6.8), so an intent
/// names the exact request it stands for without holding it.
#[must_use]
pub fn request_digest(canonical: &[u8]) -> String {
    format!("{} {}", sha256_of(canonical), canonical.len())
}

/// A credentialed request's canonical bytes: `request`'s lines 1–3, then `as {profile}` from
/// `WireRequest::as_profile` (`as anonymous` when it is `None`; never a token, a fingerprint
/// or the header), then the
/// body: `body none`; `body json {len}` and the JSON text, split at each newline, for a JSON
/// body of at most `BODY_TAPE_MAX` bytes; or `body sha256:{hex} {len}` for a larger JSON body
/// and for a file body, whose digest the pipeline sets (`WireRequest::body_digest`).
///
/// # Errors
///
/// `Replay::StreamNotTaped` for a `WireBody::Reader` with no `body_digest`; as `request`.
pub fn request_credentialed(req: &WireRequest<'_>, endpoints: &Endpoints) -> Result<Vec<u8>> {
    let mut text = head(req, endpoints)?;
    text.push_str("\nas ");
    text.push_str(req.as_profile.unwrap_or("anonymous"));
    match &req.body {
        WireBody::Empty => text.push_str("\nbody none"),
        WireBody::Bytes(bytes) => match std::str::from_utf8(bytes) {
            Ok(json) if bytes.len() <= BODY_TAPE_MAX => {
                let _ = write!(text, "\nbody json {}", bytes.len());
                for line in json.split('\n') {
                    text.push('\n');
                    text.push_str(line);
                }
            }
            _ => {
                let _ = write!(text, "\nbody {} {}", sha256_of(bytes), bytes.len());
            }
        },
        WireBody::Reader { len, .. } => {
            let Some(digest) = req.body_digest else {
                return Err(Error::replay(
                    Replay::StreamNotTaped,
                    format!(
                        "a {} with a file body and no digest is not taped",
                        req.method.as_str()
                    ),
                )
                .with_next(
                    "run it without --record or --replay: a tape names a file body by its digest",
                ));
            };
            let _ = write!(text, "\nbody {digest} {len}");
        }
    }
    Ok(text.into_bytes())
}

/// A routing decision's inputs: `route {target}` (`repo {owner}/{repo}`, `owner {owner}`,
/// `unscoped`, `identity`), `access {read|writer-read|write}`, `named {literal|target|default}`
/// with the literal's or target's text, `anonymous {0|1}`, `as {profile|-}`. One line each,
/// joined by `\n`.
#[must_use]
pub fn route_request(req: &RouteRequest) -> Vec<u8> {
    let mut out = String::with_capacity(96);
    match &req.target {
        RouteTarget::Repo(slug) => {
            let _ = write!(out, "route repo {slug}");
        }
        RouteTarget::Owner(owner) => {
            out.push_str("route owner ");
            out.push_str(owner);
        }
        RouteTarget::Unscoped => out.push_str("route unscoped"),
        RouteTarget::Identity => out.push_str("route identity"),
    }
    out.push_str("\naccess ");
    out.push_str(req.access.slug());
    match &req.named {
        Named::Default => out.push_str("\nnamed default"),
        Named::Literal(literal) => {
            let _ = write!(out, "\nnamed literal {}", literal.slug());
        }
        Named::Target(name) => {
            out.push_str("\nnamed target ");
            out.push_str(name);
        }
    }
    let _ = write!(
        out,
        "\nanonymous {}\nas {}",
        u8::from(req.anonymous),
        req.as_profile.as_deref().unwrap_or("-")
    );
    out.into_bytes()
}

/// A routing fact's question (`Fx::fact`): `fact {question} {profile}`, one line.
#[must_use]
pub fn fact_request(question: &str, profile: &str) -> Vec<u8> {
    format!("fact {} {}", one_line(question), one_line(profile)).into_bytes()
}

/// A routing fact's answer: `answer {text}`, the text through `Redactor::shapes` on one line.
#[must_use]
pub fn fact_reply(answer: &str) -> Vec<u8> {
    format!("answer {}", one_line(answer)).into_bytes()
}

/// A fact's answer read back.
///
/// # Errors
///
/// `Replay::Format` for bytes `fact_reply` did not write.
pub fn fact_decode(bytes: &[u8]) -> Result<String> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.strip_prefix("answer "))
        .filter(|answer| !answer.contains('\n'))
        .map(str::to_string)
        .ok_or_else(|| not_written_here("routing fact"))
}

// How `route_decode` reads a `refuse` line's code back.
by_slug!(
    refusal_code,
    RefusalCode: WriterAbsent,
    WriterUnusable,
    ChainExhausted,
    ChainStoppedUnusable,
    WriteNone,
    ReleaseNeedsLiteral,
    ReleaseTargetForWrite,
    AsNotInChain,
    AsNotWriter,
    ConfigInvalid,
    TierMismatch,
    MethodNotRead,
    AnonymousWriterRead,
    ProfileUnknown,
    IdentityNeedsAs,
    UnscopedNotRead,
    NotGranted,
);

// How `route_decode` reads an `anonymous` line's reason back.
by_slug!(
    anonymous_why,
    AnonymousWhy: Asked,
    ChainFellThrough,
    NoConfig
);

/// One line of a taped text: `Redactor::shapes`, with a CR or LF written as a space.
fn one_line(text: &str) -> String {
    let shaped = Redactor::shapes(text);
    if shaped.contains(['\r', '\n']) {
        shaped.replace(['\r', '\n'], " ")
    } else {
        shaped
    }
}

/// `Replay::Format`: bytes `route_reply` or `git_reply` did not write.
fn not_written_here(what: &str) -> Error {
    Error::replay(
        Replay::Format,
        format!("the tape's {what} is not one agh wrote"),
    )
    .with_next(super::tape::RECORD_AGAIN)
}

/// A decision: `send {profile} {tier}`; `anonymous {why}` (`AnonymousWhy::slug`); or
/// `refuse {code}` (`RefusalCode::slug`) followed by `why {text}`, `next {text}` and one
/// `tried {label} {presence}` line per judged step, each text through `Redactor::shapes`. So
/// a replayed refusal prints the error line the recorded one printed.
#[must_use]
pub fn route_reply(decision: &Decision) -> Vec<u8> {
    let text = match decision {
        Decision::Send { profile, tier } => format!("send {} {}", one_line(profile), tier.slug()),
        Decision::Anonymous { why } => format!("anonymous {}", why.slug()),
        Decision::Refuse(refusal) => {
            let mut text = format!(
                "refuse {}\nwhy {}\nnext {}",
                refusal.code.slug(),
                one_line(&refusal.why),
                one_line(&refusal.next)
            );
            for (step, presence) in &refusal.tried {
                let _ = write!(
                    text,
                    "\ntried {} {}",
                    one_line(step.label()),
                    presence.slug()
                );
            }
            text
        }
    };
    text.into_bytes()
}

/// A decision read back.
///
/// # Errors
///
/// `Replay::Format` for bytes `route_reply` did not write.
pub fn route_decode(bytes: &[u8]) -> Result<Decision> {
    let bad = || not_written_here("routing decision");
    let text = std::str::from_utf8(bytes).map_err(|_| bad())?;
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or_default();
    let (word, rest) = first.split_once(' ').ok_or_else(bad)?;
    let decision = match word {
        "send" => {
            let (profile, tier) = rest.rsplit_once(' ').ok_or_else(bad)?;
            let tier = Tier::parse(tier).ok_or_else(bad)?;
            if profile.is_empty() {
                return Err(bad());
            }
            Decision::Send {
                profile: profile.to_string(),
                tier,
            }
        }
        "anonymous" => Decision::Anonymous {
            why: anonymous_why(rest).ok_or_else(bad)?,
        },
        "refuse" => {
            let code = refusal_code(rest).ok_or_else(bad)?;
            let mut field = |name: &str| -> Result<String> {
                let line = lines.next().ok_or_else(bad)?;
                if line == name {
                    return Ok(String::new());
                }
                line.strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix(' '))
                    .map(str::to_string)
                    .ok_or_else(bad)
            };
            let why = field("why")?;
            let next = field("next")?;
            let mut tried = Vec::new();
            for line in lines.by_ref() {
                let step = line.strip_prefix("tried ").ok_or_else(bad)?;
                let (label, presence) = step.rsplit_once(' ').ok_or_else(bad)?;
                let step = match label {
                    "" => return Err(bad()),
                    "anonymous" => ChainStep::Anonymous,
                    name => ChainStep::Profile(name.to_string()),
                };
                let presence = Presence::from_slug(presence).ok_or_else(bad)?;
                tried.push((step, presence));
            }
            Decision::Refuse(Refusal {
                code,
                why,
                next,
                tried,
            })
        }
        _ => return Err(bad()),
    };
    if lines.next().is_some() {
        return Err(bad());
    }
    Ok(decision)
}

/// A git run's canonical bytes: `git {op} {owner}/{repo}` (`clone`, `clone-mirror`, `fetch`, `fetch-main`, `fetch-ref`,
/// `ls-remote`, `push-delete`), then the op's own words, one per line (`depth {n}`,
/// `into {ref}`, `ref {name}`, `expect {sha}`). Never the URL (the tape's header names the endpoints), the environment,
/// the grant's nonce or a path.
#[must_use]
pub fn git_request(run: &crate::git::GitRun) -> Vec<u8> {
    let slug = &run.slug;
    let text = match &run.op {
        GitOp::CheckLocal { .. } => format!("git check-local {slug}\nread-only true"),
        GitOp::Mirror { .. } => format!("git clone-mirror {slug}"),
        GitOp::Clone { depth, .. } => match depth {
            Some(depth) => format!("git clone {slug}\ndepth {depth}"),
            None => format!("git clone {slug}"),
        },
        GitOp::Fetch { .. } => format!("git fetch {slug}"),
        GitOp::FetchMain { into, .. } => format!("git fetch-main {slug}\ninto {into}"),
        GitOp::FetchRef {
            reference,
            into,
            anonymous,
            ..
        } => format!("git fetch-ref {slug}\nref {reference}\ninto {into}\nanonymous {anonymous}"),
        GitOp::FetchBatch {
            batch, anonymous, ..
        } => {
            let mut text = format!("git fetch-batch {slug}\nanonymous {anonymous}");
            for row in batch.rows() {
                let _ = write!(
                    text,
                    "\nsource {}\ninto {}\nexpected {}",
                    row.source, row.into, row.expected_oid
                );
            }
            text
        }
        GitOp::LsRemote {
            reference,
            anonymous,
        } => {
            let mut text = match reference {
                Some(reference) => format!("git ls-remote {slug}\nref {reference}"),
                None => format!("git ls-remote {slug}"),
            };
            if *anonymous {
                text.push_str("\nanonymous true");
            }
            text
        }
        GitOp::PushDelete { reference, expect } => {
            format!("git push-delete {slug}\nref {reference}\nexpect {expect}")
        }
        GitOp::PushRefs { batch, dry_run, .. } => {
            let mut text = format!("git push-refs {slug}\ndry-run {dry_run}");
            for update in batch.updates() {
                let _ = write!(
                    text,
                    "\nref {}\nbefore {}\nafter {}",
                    update.reference, update.before, update.after
                );
            }
            text
        }
    };
    text.into_bytes()
}

/// One stream's pieces for `git_reply`: `{name} {n}`, then each piece, shaped.
fn git_stream(out: &mut Vec<u8>, name: &str, bytes: &[u8]) {
    let count = bytes.iter().filter(|&&b| b == b'\n').count() + 1;
    let _ = write!(out, "\n{name} {count}");
    for piece in bytes.split(|&b| b == b'\n') {
        out.push(b'\n');
        out.extend_from_slice(&super::tape::shaped(piece));
    }
}

/// A git run's answer: `exit {code}` or `exit signal`, then `stdout {n}` and stdout's `n`
/// pieces split at each `\n`, then `stderr {m}` and stderr's `m` pieces, each piece through
/// `Redactor::shapes`. An empty stream is one empty piece.
#[must_use]
pub fn git_reply(out: &super::child::ChildOut) -> Vec<u8> {
    let mut bytes = match out.code {
        Some(code) => format!("exit {code}"),
        None => "exit signal".to_string(),
    }
    .into_bytes();
    git_stream(&mut bytes, "stdout", &out.stdout);
    git_stream(&mut bytes, "stderr", &out.stderr);
    bytes
}

/// `git_reply`'s bytes read back.
///
/// # Errors
///
/// `Replay::Format` for bytes `git_reply` did not write.
#[cfg(any(unix, test))]
pub(crate) fn git_decode(bytes: &[u8]) -> Result<super::child::ChildOut> {
    let bad = || not_written_here("git answer");
    let mut lines = bytes.split(|&b| b == b'\n');
    let exit = lines
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .and_then(|line| line.strip_prefix("exit "))
        .ok_or_else(bad)?;
    let code = match exit {
        "signal" => None,
        code => Some(code.parse::<i32>().map_err(|_| bad())?),
    };
    let mut stream = |name: &str| -> Result<Vec<u8>> {
        let line = lines.next().ok_or_else(bad)?;
        let count: u64 = std::str::from_utf8(line)
            .ok()
            .and_then(|line| line.strip_prefix(name))
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(crate::path::digits)
            .filter(|n| *n > 0)
            .ok_or_else(bad)?;
        let mut out = Vec::new();
        for i in 0..count {
            let piece = lines.next().ok_or_else(bad)?;
            if i > 0 {
                out.push(b'\n');
            }
            out.extend_from_slice(piece);
        }
        Ok(out)
    };
    let stdout = stream("stdout")?;
    let stderr = stream("stderr")?;
    if lines.next().is_some() {
        return Err(bad());
    }
    Ok(super::child::ChildOut {
        code,
        stdout,
        stderr,
    })
}

// How `git_answer` reads a `failed` line's side effect back.
#[cfg(any(unix, test))]
by_slug!(side_effect, SideEffect: None, Unknown, Applied);

/// A git run that ended with no exit to tape: `child::run`'s error, which is could_not_run,
/// `Sub::Local` (the run could not start, could not be waited for, or did not finish in its
/// budget). `failed {side effect}` (`none`, `unknown` or `applied`), then `why {text}`, the
/// text through `Redactor::shapes` with a CR or LF written as a space. `git_answer` reads it
/// back, so a replay ends where the recorded run did, with the same side effect.
#[cfg(any(unix, test))]
pub(crate) fn git_failure(e: &Error) -> Vec<u8> {
    format!("failed {}\nwhy {}", e.side_effect.slug(), one_line(&e.why)).into_bytes()
}

/// A taped git answer read back: `git_reply`'s bytes as the run's output (`git_decode`),
/// `git_failure`'s as its error.
///
/// # Errors
///
/// `Replay::Format` for bytes neither wrote.
#[cfg(any(unix, test))]
pub(crate) fn git_answer(
    bytes: &[u8],
) -> Result<std::result::Result<super::child::ChildOut, Error>> {
    let Some(rest) = bytes.strip_prefix(b"failed ") else {
        return git_decode(bytes).map(Ok);
    };
    let bad = || not_written_here("git answer");
    let text = std::str::from_utf8(rest).map_err(|_| bad())?;
    let (effect, why) = text.split_once('\n').ok_or_else(bad)?;
    let side_effect = side_effect(effect).ok_or_else(bad)?;
    let why = why.strip_prefix("why ").ok_or_else(bad)?;
    if why.contains('\n') {
        return Err(bad());
    }
    Ok(Err(Error::local(why).with_side_effect(side_effect)))
}

/// The line a streamed reply holds in place of its body: the sidecar's digest and length.
#[cfg(unix)]
const BODY_FILE: &str = "body-file";

/// A streamed answer on a credentialed tape: `status {status}`, the kept headers, then
/// `body-file sha256:{hex} {len}`, the sidecar that holds its bytes (`{tape}.d/{hex}`).
#[cfg(unix)]
pub(crate) fn stream_reply(
    status: u16,
    headers: &[(String, String)],
    hex: &str,
    len: u64,
) -> Vec<u8> {
    let mut text = format!("status {status}\n");
    for (name, value) in headers {
        text.push_str(name);
        text.push(' ');
        text.push_str(value);
        text.push('\n');
    }
    let _ = write!(text, "{BODY_FILE} sha256:{hex} {len}");
    text.into_bytes()
}

/// A streamed answer's head, read back from `stream_reply`'s bytes.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct StreamHead {
    /// The status.
    pub(crate) status: u16,
    /// The kept headers, in order.
    pub(crate) headers: Vec<(String, String)>,
    /// The sidecar's lowercase hex SHA-256.
    pub(crate) hex: String,
    /// The body's length.
    pub(crate) len: u64,
}

/// `stream_reply`'s bytes, or `failure`'s, read back.
///
/// # Errors
///
/// `Replay::Format` for bytes neither wrote.
#[cfg(unix)]
pub(crate) fn stream_decode(bytes: &[u8]) -> Result<std::result::Result<StreamHead, WireError>> {
    if let Some(rest) = bytes.strip_prefix(b"failed ") {
        return decode_failure(rest).map(Err);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| not_a_reply("a stream's head not UTF-8"))?;
    let mut lines = text.split('\n');
    let last = lines
        .next_back()
        .and_then(|line| line.strip_prefix(BODY_FILE))
        .and_then(|rest| rest.strip_prefix(" sha256:"))
        .ok_or_else(|| not_a_reply("a streamed answer without its body-file line"))?;
    let (hex, len) = last
        .split_once(' ')
        .ok_or_else(|| not_a_reply("a body-file line without its length"))?;
    let well_formed = aterm_codec::hex::is_lower(hex, 64);
    let len = crate::path::digits(len)
        .filter(|_| well_formed)
        .ok_or_else(|| not_a_reply("a body-file line that is not a sha256 digest and a length"))?;
    let status = lines
        .next()
        .and_then(|line| line.strip_prefix("status "))
        .and_then(crate::path::digits::<u16>)
        .ok_or_else(|| not_a_reply("a status that is no number"))?;
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line
            .split_once(' ')
            .ok_or_else(|| not_a_reply("a header line without a value"))?;
        headers.push((name.to_string(), value.to_string()));
    }
    Ok(Ok(StreamHead {
        status,
        headers,
        hex: hex.to_string(),
        len,
    }))
}

/// Encodes `status`, `headers`, and `body` into canonical tape reply bytes, replacing bodies over [`BODY_TAPE_MAX`] with a `body-digest` header.
#[must_use]
pub fn reply(status: u16, headers: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut text = format!("status {status}\n");
    for (name, value) in headers {
        text.push_str(name);
        text.push(' ');
        text.push_str(value);
        text.push('\n');
    }
    if body.len() > BODY_TAPE_MAX {
        let _ = write!(text, "{BODY_DIGEST} {} {}\n\n", sha256_of(body), body.len());
        return text.into_bytes();
    }
    text.push('\n');
    let mut bytes = text.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// The phase's word on a `failed` line.
const fn phase_word(phase: Phase) -> &'static str {
    match phase {
        Phase::NotSent => "not_sent",
        Phase::Sent => "sent",
        Phase::Answered => "answered",
    }
}

/// Encodes `error` as a `failed {phase} {kind} {detail}` line with shape-redacted detail.
#[must_use]
pub fn failure(error: &WireError) -> Vec<u8> {
    format!(
        "failed {} {} {}",
        phase_word(error.phase),
        error.kind.slug(),
        Redactor::shapes(&error.detail)
    )
    .into_bytes()
}

/// `Replay::Format`: bytes `reply` and `failure` did not write.
fn not_a_reply(what: &str) -> Error {
    Error::replay(
        Replay::Format,
        format!("the tape's reply is not one agh wrote: {what}"),
    )
    .with_next(super::tape::RECORD_AGAIN)
}

/// A `failed` line's rest read back.
fn decode_failure(rest: &[u8]) -> Result<WireError> {
    let rest = std::str::from_utf8(rest).map_err(|_| not_a_reply("a failure that is not UTF-8"))?;
    let mut words = rest.splitn(3, ' ');
    let (Some(phase), Some(kind), Some(detail)) = (words.next(), words.next(), words.next()) else {
        return Err(not_a_reply("a failure without its phase, kind and detail"));
    };
    let phase = match phase {
        "not_sent" => Phase::NotSent,
        "sent" => Phase::Sent,
        "answered" => Phase::Answered,
        _ => return Err(not_a_reply("a failure of no phase agh names")),
    };
    let kind = wire_kind(kind).ok_or_else(|| not_a_reply("a failure of no wire kind agh names"))?;
    Ok(WireError {
        phase,
        kind,
        detail: detail.to_string(),
    })
}

/// A reply read back.
///
/// # Errors
///
/// `Replay::Format` for bytes `reply` and `failure` did not write; `Replay::BodyNotTaped` for a
/// `body-digest` reply.
pub fn decode(bytes: &[u8]) -> Result<std::result::Result<Reply, WireError>> {
    if let Some(rest) = bytes.strip_prefix(b"failed ") {
        return decode_failure(rest).map(Err);
    }
    if !bytes.starts_with(b"status ") {
        return Err(not_a_reply("its first line is neither status nor failed"));
    }
    let Some(sep) = bytes.windows(2).position(|pair| pair == b"\n\n") else {
        return Err(not_a_reply("no empty line ends its headers"));
    };
    let body_at = sep + 2;
    let mut lines = bytes.get(..sep).unwrap_or_default().split(|&b| b == b'\n');
    let status = lines
        .next()
        .and_then(|line| line.strip_prefix(b"status "))
        .filter(|code| {
            !code.is_empty()
                && code.iter().all(u8::is_ascii_digit)
                && (code.len() == 1 || !code.starts_with(b"0"))
        })
        .and_then(|code| std::str::from_utf8(code).ok())
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| not_a_reply("a status that is no number"))?;
    let mut headers = Vec::new();
    for line in lines {
        let line = std::str::from_utf8(line).map_err(|_| not_a_reply("a header not UTF-8"))?;
        let (name, value) = line
            .split_once(' ')
            .ok_or_else(|| not_a_reply("a header line without a value"))?;
        if name == BODY_DIGEST {
            let size = value.rsplit(' ').next().unwrap_or_default();
            return Err(Error::replay(
                Replay::BodyNotTaped,
                format!(
                    "the tape holds this answer's body by its digest ({size} bytes): replay \
                     needs a body of at most 1 MiB"
                ),
            )
            .with_next("run it live, with no --replay: a tape holds no answer over 1 MiB"));
        }
        headers.push((name.to_string(), value.to_string()));
    }
    Ok(Ok(Reply {
        status,
        headers,
        body: bytes.get(body_at..).unwrap_or_default().to_vec(),
    }))
}

/// A whole answer, as a tape holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// The status.
    pub status: u16,
    /// The kept headers, in order.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Reply {
    /// The answer as the pipeline reads it: the body as an in-memory reader.
    #[must_use]
    pub fn into_response(self) -> WireResponse {
        WireResponse {
            status: self.status,
            headers: self.headers,
            body: Box::new(Cursor::new(self.body)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BODY_TAPE_MAX, request_credentialed, request_digest};
    use crate::error::{Kind, Replay, Sub};
    use crate::identity::Credential;
    use crate::policy::Tier;
    use crate::wire::door::AuthHeader;
    use crate::wire::{Deadlines, Endpoints, Method, WireBody, WireRequest};

    /// `method` to `url` on github.com's endpoints, as the profile `dev`.
    fn as_dev<'a>(method: Method, url: &str) -> WireRequest<'a> {
        let mut req = WireRequest::new(method, url, Vec::new(), 1 << 20, Deadlines::JSON);
        req.as_profile = Some("dev");
        req
    }

    /// The credentialed canonical bytes, as text.
    fn text(req: &WireRequest<'_>) -> String {
        let bytes = request_credentialed(req, &Endpoints::github_com()).expect("canonical");
        String::from_utf8(bytes).expect("UTF-8")
    }

    /// The digest is the SHA-256 test vector's, then the size.
    #[test]
    fn a_request_digest_is_sha256_then_the_size() {
        assert_eq!(
            request_digest(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad 3"
        );
    }

    /// The profile is named, the credential never: an `authorization` header among the
    /// headers, and the `auth` the pipeline built, leave the bytes as they are without them.
    #[test]
    fn a_credentialed_request_names_its_profile_and_never_its_credential() {
        let e = Endpoints::github_com();
        let token = agh_fake::token("canon-credentialed");
        let cred = Credential::for_tests("dev", Tier::Dev, token.as_bytes());
        let header = AuthHeader::for_origin_for_tests(&cred, &e, e.api()).expect("a header");
        let plain = as_dev(Method::Get, "https://api.github.com/user");
        let mut carried = as_dev(Method::Get, "https://api.github.com/user");
        carried
            .headers
            .push(("Authorization".to_string(), format!("Bearer {token}")));
        carried.auth = Some(&header);
        assert_eq!(text(&plain), "GET /user\norigin api\nas dev\nbody none");
        assert_eq!(text(&carried), text(&plain));
        let mut anonymous = as_dev(Method::Get, "https://api.github.com/user");
        anonymous.as_profile = None;
        assert_eq!(
            text(&anonymous),
            "GET /user\norigin api\nas anonymous\nbody none"
        );
    }

    /// A JSON body of at most `BODY_TAPE_MAX` bytes is written out, one line per newline.
    #[test]
    fn a_small_json_body_is_written_line_by_line() {
        let body = b"{\"a\":1}\n{\"b\":2}";
        let mut req = as_dev(Method::Post, "https://api.github.com/repos/o/r/releases");
        req.body = WireBody::Bytes(body);
        assert_eq!(
            text(&req),
            "POST /repos/o/r/releases\norigin api\nas dev\nbody json 15\n{\"a\":1}\n{\"b\":2}"
        );
        let at_max = vec![b' '; BODY_TAPE_MAX];
        req.body = WireBody::Bytes(&at_max);
        assert!(
            text(&req).contains(&format!("\nbody json {BODY_TAPE_MAX}\n")),
            "a body of exactly BODY_TAPE_MAX bytes is written out"
        );
    }

    /// A body over `BODY_TAPE_MAX`, or one that is not UTF-8, is named by its digest and size.
    #[test]
    fn a_large_or_binary_body_is_named_by_its_digest() {
        let large = vec![b' '; BODY_TAPE_MAX + 1];
        let binary = [0xff, 0xfe, b'a'];
        for body in [&large[..], &binary[..]] {
            let mut req = as_dev(Method::Patch, "https://api.github.com/repos/o/r");
            req.body = WireBody::Bytes(body);
            assert_eq!(
                text(&req),
                format!(
                    "PATCH /repos/o/r\norigin api\nas dev\nbody {}",
                    request_digest(body)
                )
            );
        }
    }

    #[test]
    fn exact_fetch_identity_keeps_refs_and_auth_mode_but_not_local_paths() {
        use crate::git::GitRun;
        use std::path::Path;

        let slug = crate::slug::Slug::new("owner", "repo").unwrap();
        let request = |path: &str, source: &str, into: &str, anonymous: bool| {
            super::git_request(&GitRun::fetch_ref(
                &slug,
                Path::new(path),
                source,
                into,
                anonymous,
            ))
        };
        let expected = request("/first/checkout", "refs/heads/main", "refs/agh/exact", true);
        assert_eq!(
            expected,
            b"git fetch-ref owner/repo\nref refs/heads/main\ninto refs/agh/exact\nanonymous true"
        );
        assert_eq!(
            expected,
            request(
                "/another/checkout",
                "refs/heads/main",
                "refs/agh/exact",
                true
            )
        );
        for (source, into, anonymous) in [
            ("refs/tags/v1", "refs/agh/exact", true),
            ("refs/heads/main", "refs/agh/other", true),
            ("refs/heads/main", "refs/agh/exact", false),
        ] {
            assert_ne!(
                expected,
                request("/first/checkout", source, into, anonymous)
            );
        }
    }

    #[test]
    fn batch_fetch_identity_binds_the_snapshot_and_auth_mode_without_local_paths() {
        use crate::git::{FetchBatch, FetchRow, GitRun};
        let slug = crate::slug::Slug::new("owner", "repo").unwrap();
        let request = |path: &str, expected: &str, anonymous| {
            let batch = FetchBatch::new(vec![FetchRow {
                source: "refs/heads/main".into(),
                into: "refs/agh/main".into(),
                expected_oid: expected.into(),
            }])
            .unwrap();
            super::git_request(&GitRun::fetch_batch(
                &slug,
                std::path::Path::new(path),
                batch,
                anonymous,
            ))
        };
        let expected = "a".repeat(40);
        let first = request("/one", &expected, true);
        assert_eq!(first, request("/two", &expected, true));
        assert_ne!(first, request("/one", &"b".repeat(40), true));
        assert_ne!(first, request("/one", &expected, false));
    }

    #[test]
    fn native_push_identity_binds_each_old_new_ref_and_dry_run_but_not_source_path() {
        use crate::git::GitRun;
        use crate::github::refs::{AtomicRefBatch, RefUpdate};
        let slug = crate::slug::Slug::new("owner", "repo").unwrap();
        let request = |dir: &str, reference: &str, before: char, after: char, dry| {
            let batch = AtomicRefBatch::new(vec![RefUpdate {
                reference: reference.into(),
                before: before.to_string().repeat(40),
                after: after.to_string().repeat(40),
            }])
            .unwrap();
            super::git_request(&GitRun::push_refs(
                &slug,
                std::path::Path::new(dir),
                batch,
                dry,
            ))
        };
        let expected = request("/first", "refs/heads/main", '0', 'a', false);
        assert_eq!(
            expected,
            request("/other", "refs/heads/main", '0', 'a', false)
        );
        for (reference, before, after, dry) in [
            ("refs/tags/v1", '0', 'a', false),
            ("refs/heads/main", 'b', 'a', false),
            ("refs/heads/main", '0', 'c', false),
            ("refs/heads/main", '0', 'a', true),
        ] {
            assert_ne!(expected, request("/first", reference, before, after, dry));
        }
    }

    /// `git_decode` reads back what `git_reply` wrote, and refuses what it did not.
    #[test]
    fn git_replies_round_trip() {
        use super::super::child::ChildOut;
        use super::{git_decode, git_reply};
        let out = |code: Option<i32>, stdout: &[u8], stderr: &[u8]| ChildOut {
            code,
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        };
        for answer in [
            out(Some(0), b"", b""),
            out(Some(0), b"abc\n", b""),
            out(Some(1), b"", b"one\ntwo\nthree"),
            out(None, b"x", b"\n"),
        ] {
            assert_eq!(
                git_decode(&git_reply(&answer)).expect("decodes"),
                answer,
                "{answer:?}"
            );
        }
        assert_eq!(
            git_reply(&out(Some(0), b"", b"")),
            b"exit 0\nstdout 1\n\nstderr 1\n"
        );
        let token = agh_fake::token("canon-git");
        let shaped = git_decode(&git_reply(&out(
            Some(128),
            b"",
            format!("fatal: {token}\n").as_bytes(),
        )))
        .expect("decodes");
        assert_eq!(shaped.stderr, b"fatal: [redacted]\n");
        let run = aterm_codec::base64::encode(b"agh:canon-git-1").expect("base64");
        assert_eq!(run.len(), 20);
        assert!(run.bytes().any(|b| !b.is_ascii_alphabetic()), "{run}");
        let basic = git_decode(&git_reply(&out(
            Some(128),
            b"",
            format!("remote: Basic {run}\nfatal: denied").as_bytes(),
        )))
        .expect("decodes");
        let stderr = String::from_utf8(basic.stderr).expect("UTF-8");
        assert_eq!(stderr, "remote: Basic [redacted]\nfatal: denied");
        for junk in [
            &b""[..],
            b"exit 0",
            b"exit x\nstdout 1\n\nstderr 1\n",
            b"exit 0\nstdout 2\n\nstderr 1\n",
            b"exit 0\nstdout 1\n\nstderr 1\n\nmore",
        ] {
            let refused = git_decode(junk).expect_err("not git_reply's");
            assert_eq!(refused.sub, Sub::Replay(Replay::Format), "{junk:?}");
        }
    }

    /// A git run that ended with no exit is taped as its error and read back as that error,
    /// its side effect kept and its why shaped; any other `failed` bytes are `Replay::Format`.
    #[test]
    fn git_failures_round_trip() {
        use super::{git_answer, git_failure};
        use crate::error::SideEffect;

        let token = agh_fake::token("canon-git-failure");
        for effect in [SideEffect::None, SideEffect::Unknown, SideEffect::Applied] {
            let failed =
                crate::error::Error::local(format!("/usr/bin/git did not finish\nwith {token}"))
                    .with_side_effect(effect);
            let back = git_answer(&git_failure(&failed))
                .expect("decodes")
                .expect_err("a failure");
            assert_eq!(back.kind, Kind::CouldNotRun);
            assert_eq!(back.sub, Sub::Local);
            assert_eq!(back.side_effect, effect);
            assert_eq!(back.why, "/usr/bin/git did not finish with [redacted]");
        }
        let ran = super::super::child::ChildOut {
            code: Some(0),
            stdout: b"ok".to_vec(),
            stderr: Vec::new(),
        };
        let answer = git_answer(&super::git_reply(&ran)).expect("decodes");
        assert_eq!(answer.expect("an exit"), ran);
        for junk in [
            &b"failed"[..],
            b"failed maybe\nwhy x",
            b"failed none",
            b"failed none\nwhy x\nmore",
            b"failed none\nhow x",
        ] {
            let refused = git_answer(junk).expect_err("not git_failure's");
            assert_eq!(refused.sub, Sub::Replay(Replay::Format), "{junk:?}");
        }
    }

    /// A file body is named by the digest the pipeline set; with none, it is not taped.
    #[test]
    fn a_file_body_is_named_by_its_digest_or_refused() {
        let mut bytes: &[u8] = b"abc";
        let mut req = as_dev(Method::Post, "https://uploads.github.com/repos/o/r/assets");
        req.body = WireBody::Reader {
            len: 3,
            reader: &mut bytes,
        };
        req.body_digest = Some("sha256:00ff");
        assert_eq!(
            text(&req),
            "POST /repos/o/r/assets\norigin uploads\nas dev\nbody sha256:00ff 3"
        );
        req.body_digest = None;
        let refused = request_credentialed(&req, &Endpoints::github_com()).expect_err("no digest");
        assert_eq!(
            (refused.kind, refused.sub.clone()),
            (Kind::CouldNotRun, Sub::Replay(Replay::StreamNotTaped))
        );
    }

    /// A redirect off the API keeps no query: codeload's `?token=` and a signed blob URL's
    /// `?sig=` are bearer secrets. One on the API origin is kept as it is.
    #[test]
    fn a_location_off_the_api_keeps_no_query() {
        let e = Endpoints::github_com();
        let location = |url: &str| {
            let kept = super::keep(&e, vec![("Location".to_string(), url.to_string())]);
            kept.into_iter().map(|(_, value)| value).collect::<Vec<_>>()
        };
        assert_eq!(
            location("https://codeload.github.com/o/r/legacy.tar.gz/refs/heads/main?token=ABCDEF"),
            ["https://codeload.github.com/o/r/legacy.tar.gz/refs/heads/main?signed=redacted"]
        );
        assert_eq!(
            location("https://pipelines.example.net/logs/1.zip?sv=2025&se=9&sig=c2lnbmF0dXJl"),
            ["https://pipelines.example.net/logs/1.zip?signed=redacted"]
        );
        let api = "https://api.github.com/repositories/9/releases?page=2&per_page=100";
        assert_eq!(location(api), [api]);
        assert_eq!(
            location("https://codeload.github.com/o/r/tar.gz/main"),
            ["https://codeload.github.com/o/r/tar.gz/main"]
        );
    }
}
