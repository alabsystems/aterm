// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! An HTTP answer or a wire failure, as a kind and a sub-kind (design §7.2).

use crate::error::{Error, Forbid, GithubBody, Kind, SideEffect, Sub};
use crate::retry::Idempotency;
use crate::wire::{Method, Phase, WireError, header_in};

#[inline]
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    header_in(headers, name).map(str::trim)
}

#[inline]
fn number(headers: &[(String, String)], name: &str) -> Option<u64> {
    header(headers, name).and_then(|v| v.parse::<u64>().ok())
}

#[inline]
fn spent(headers: &[(String, String)]) -> bool {
    number(headers, "x-ratelimit-remaining") == Some(0)
}

fn primary(error: Error, headers: &[(String, String)]) -> Error {
    let error = error.with_sub(Sub::Primary);
    match number(headers, "x-ratelimit-reset") {
        Some(reset) => error.with_reset(reset),
        None => error,
    }
}

fn secondary(error: Error, headers: &[(String, String)]) -> Error {
    let error = error.with_sub(Sub::Secondary);
    match number(headers, "retry-after") {
        Some(secs) => error.with_retry_after(secs),
        None => error,
    }
}

#[inline]
fn scopes(text: &str) -> impl Iterator<Item = &str> {
    text.split(',').map(str::trim).filter(|s| !s.is_empty())
}

/// The first scope `x-accepted-oauth-scopes` names that `x-oauth-scopes` lacks.
pub(crate) fn missing_scope(headers: &[(String, String)]) -> Option<String> {
    let held = header(headers, "x-oauth-scopes")?;
    let accepted = header(headers, "x-accepted-oauth-scopes")?;
    scopes(accepted)
        .find(|want| !scopes(held).any(|have| have == *want))
        .map(str::to_string)
}

/// The `next` that names a missing OAuth scope.
#[inline]
pub(crate) fn scope_next(scope: &str) -> String {
    format!("the token lacks the {scope} scope; a person re-issues it with {scope}")
}

fn forbidden(error: Error, headers: &[(String, String)], body: &GithubBody) -> Error {
    if let Some(sso) = header(headers, "x-github-sso")
        && sso.starts_with("required")
    {
        let url = sso
            .split_once("url=")
            .map(|(_, url)| url.trim().to_string())
            .filter(|url| !url.is_empty());
        return error.with_sub(Sub::Forbid(Forbid::Sso { url }));
    }
    let not_accessible = body
        .message
        .as_deref()
        .is_some_and(|m| m.starts_with("Resource not accessible by"));
    if not_accessible || body.accepted_permissions.is_some() {
        return error.with_sub(Sub::Forbid(Forbid::Permission {
            accepted: body.accepted_permissions.clone(),
        }));
    }
    error.with_sub(Sub::Forbid(Forbid::Other))
}

/// `None` for 2xx and 304; otherwise the error (design §7.2's table).
#[must_use]
pub fn answer(
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    method: Method,
    idempotency: Idempotency,
) -> Option<Error> {
    let _ = (method, idempotency);
    if (200..300).contains(&status) || status == 304 {
        return None;
    }
    let github = GithubBody::parse(body).with_headers(headers);
    let why = match github.message.as_deref() {
        Some(message) if !message.is_empty() => format!("GitHub answered {status}: {message}"),
        _ => format!("GitHub answered {status}"),
    };
    let moved = || Sub::Moved {
        to: header(headers, "location").unwrap_or_default().to_string(),
    };
    let of = |kind: Kind| Error::new(kind, why.clone());
    let error = match status {
        301 => of(Kind::Conflict).with_sub(moved()),
        300..=399 => of(Kind::CouldNotRun).with_sub(Sub::Protocol),
        400 => of(Kind::Invalid),
        401 => of(Kind::Unauthenticated),
        403 if spent(headers) => primary(of(Kind::RateLimited), headers),
        403 if header(headers, "retry-after").is_some()
            || github
                .message
                .as_deref()
                .is_some_and(|m| m.to_ascii_lowercase().contains("secondary rate limit")) =>
        {
            secondary(of(Kind::RateLimited), headers)
        }
        403 => forbidden(of(Kind::Forbidden), headers, &github),
        404 | 410 => of(Kind::NotFound),
        405 | 409 => of(Kind::Conflict),
        422 if github
            .errors
            .iter()
            .any(|e| e.code.as_deref() == Some("already_exists")) =>
        {
            of(Kind::Conflict).with_sub(Sub::AlreadyExists)
        }
        429 if spent(headers) => primary(of(Kind::RateLimited), headers),
        429 => secondary(of(Kind::RateLimited), headers),
        451 => of(Kind::Forbidden).with_sub(Sub::Forbid(Forbid::Blocked)),
        500..=599 => of(Kind::Server),
        402..=499 => of(Kind::Invalid),
        _ => of(Kind::CouldNotRun).with_sub(Sub::Protocol),
    };
    let error = if matches!(error.kind, Kind::Forbidden | Kind::NotFound)
        && (status == 403 || status == 404)
        && let Some(scope) = missing_scope(headers)
    {
        error.with_next(scope_next(&scope))
    } else {
        error
    };
    Some(error.with_github(github))
}

/// A wire failure: NotSent → CouldNotRun `Wire(kind)`.
#[must_use]
pub fn wire(error: &WireError, method: Method, idempotency: Idempotency) -> Error {
    let why = format!("{}: {}", error.kind.slug(), error.detail);
    let sub = Sub::Wire(error.kind);
    let reached = error.phase != Phase::NotSent;
    if reached && !method.is_read() && idempotency == Idempotency::OneShot {
        return Error::new(Kind::OutcomeUnknown, why)
            .with_sub(sub)
            .with_side_effect(SideEffect::Unknown);
    }
    Error::could_not_run(why).with_sub(sub)
}
