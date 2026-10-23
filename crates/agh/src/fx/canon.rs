// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Canonical tape encoding and decoding for GitHub HTTP requests, responses, and wire failures.

use std::io::Cursor;

use crate::error::{Error, Replay, Result};
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

/// The query a CDN URL is written with: its signature is a bearer secret with an expiry.
const SIGNED_REDACTED: &str = "signed=redacted";

/// The line a reply over `BODY_TAPE_MAX` holds in place of its body.
const BODY_DIGEST: &str = "body-digest";

/// Every wire kind, by its slug: how `decode` reads a `failed` line's kind back.
const WIRE_KINDS: [WireKind; 13] = [
    WireKind::Dns,
    WireKind::Connect,
    WireKind::Proxy,
    WireKind::Tls,
    WireKind::Trust,
    WireKind::Timeout,
    WireKind::Idle,
    WireKind::Reset,
    WireKind::Truncated,
    WireKind::TooLarge,
    WireKind::Protocol,
    WireKind::Revoked,
    WireKind::Local,
];

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
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect()
}

/// Filters `headers` to [`KEPT_HEADERS`] with lowercased names, redacting queries on `location` URLs targeting `endpoints.asset_hosts()`.
#[must_use]
pub fn keep(endpoints: &Endpoints, headers: Vec<(String, String)>) -> Vec<(String, String)> {
    cut(headers)
        .into_iter()
        .map(|(name, value)| {
            if name != "location" {
                return (name, value);
            }
            let value = signed_redacted(endpoints, &value).unwrap_or(value);
            (name, value)
        })
        .collect()
}

/// Replaces `url`'s query with `signed=redacted` when its origin is an asset host.
fn signed_redacted(endpoints: &Endpoints, url: &str) -> Option<String> {
    let (origin, path_and_query) = Origin::parse(url)?;
    if !endpoints.asset_hosts().contains(&origin) {
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
fn sorted_query(query: &str) -> String {
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
    pairs
        .iter()
        .map(|(_, _, pair)| *pair)
        .collect::<Vec<_>>()
        .join("&")
}

/// The next of a request M1's tapes cannot hold: every run of it would be refused the same.
const ANONYMOUS_READS: &str =
    "run it without --record or --replay: M1's tapes hold anonymous reads alone";

/// Encodes `req` into canonical tape request bytes (method, path and sorted or redacted query, origin role, sorted headers excluding `user-agent`, `as anonymous`, and `body none`).
///
/// # Errors
///
/// `Replay::StreamNotTaped` for a body other than `WireBody::Empty`, or a request with `auth`
/// set (M1's tapes are anonymous); CouldNotRun, `Sub::Protocol`, for a URL on none of
/// `endpoints`' origins.
pub fn request(req: &WireRequest<'_>, endpoints: &Endpoints) -> Result<Vec<u8>> {
    if !matches!(req.body, WireBody::Empty) {
        return Err(Error::replay(
            Replay::StreamNotTaped,
            format!(
                "a {} with a body is not taped: M1's tapes hold reads alone",
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
            "a request that carries a credential is not taped: M1's tapes are anonymous",
        )
        .with_next(ANONYMOUS_READS));
    }
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
    let mut headers: Vec<(String, &str)> = req
        .headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.as_str()))
        .filter(|(name, _)| name != "user-agent")
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
    text.push_str("\nas anonymous\nbody none");
    Ok(text.into_bytes())
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
        let digest = aterm_digest::Sha256::digest(body);
        text.push_str(&format!(
            "{BODY_DIGEST} sha256:{} {}\n\n",
            aterm_codec::hex::encode_lower(&digest),
            body.len()
        ));
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
    let kind = WIRE_KINDS
        .iter()
        .copied()
        .find(|k| k.slug() == kind)
        .ok_or_else(|| not_a_reply("a failure of no wire kind agh names"))?;
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
    let mut lines = Vec::new();
    let mut at = 0;
    let body_at = loop {
        let rest = bytes.get(at..).unwrap_or_default();
        let Some(end) = rest.iter().position(|&b| b == b'\n') else {
            return Err(not_a_reply("no empty line ends its headers"));
        };
        let line = rest.get(..end).unwrap_or_default();
        at += end + 1;
        if line.is_empty() {
            break at;
        }
        lines.push(line);
    };
    let mut lines = lines.into_iter();
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
