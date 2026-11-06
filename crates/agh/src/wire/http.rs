// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Production HTTP transport backed by `aterm-http`.

use std::ffi::OsString;
use std::sync::Arc;

use aterm_http::{Body, Client, ProxyMode, SendError, Trust};
use aterm_uds::secret::Secret;

use crate::identity::EnvSource;
use crate::policy::PermitTarget;
use crate::wire::{
    Deadlines, Method, Origin, Phase, WireBody, WireError, WireKind, WireRequest, WireResponse,
    path_only, wire_kind_of,
};

/// Sends through aterm-http with the platform trust store and the environment's proxy.
pub struct HttpTransport {
    client: Client,
    env: Arc<dyn EnvSource + Send + Sync>,
}

impl HttpTransport {
    /// Creates an `HttpTransport` using platform TLS verification and environment proxy settings.
    #[must_use]
    pub fn new(env: Arc<dyn EnvSource + Send + Sync>) -> HttpTransport {
        HttpTransport {
            client: Client::new(
                Trust::PlatformVerifier,
                ProxyMode::Environment,
                Deadlines::JSON.backstop,
            ),
            env,
        }
    }

    /// Creates a direct loopback `HttpTransport` with an empty TLS root store.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn direct(env: Arc<dyn EnvSource + Send + Sync>) -> HttpTransport {
        HttpTransport {
            client: Client::new(
                Trust::Roots(Vec::new()),
                ProxyMode::Direct,
                Deadlines::JSON.backstop,
            ),
            env,
        }
    }
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpTransport")
    }
}

struct EnvAdapter<'a>(&'a (dyn EnvSource + Send + Sync));

impl EnvSource for EnvAdapter<'_> {
    fn var_os(&self, name: &str) -> Option<OsString> {
        self.0.var_os(name)
    }
}

const fn refused(detail: String) -> WireError {
    WireError {
        phase: Phase::NotSent,
        kind: WireKind::Local,
        detail,
    }
}

/// What a permit's target is called in a refusal: `{owner}/{repo}`, or `/user`.
fn target_text(target: &PermitTarget) -> String {
    match target {
        PermitTarget::Repo(slug) => slug.to_string(),
        PermitTarget::Identity => "/user".to_string(),
    }
}

/// Checks 2–4 of a write's guard (plan §4.6.4): a permit, whose target covers the URL's path on
/// a credential origin, and whose profile is the credential's. A read never reaches it.
///
/// The transport holds no endpoints, so a URL's origin is judged by the credential the
/// pipeline built for it: `AuthHeader::for_origin` builds one only for the api and uploads
/// origins, and a write's URL must be that credential's own origin. The pipeline holds the
/// Identity target to the api origin before it builds the request (`client::write_target`).
fn write_guard(req: &WireRequest<'_>) -> Option<WireError> {
    let method = req.method.as_str();
    let Some(permit) = req.permit else {
        return Some(refused(format!(
            "{method} needs a write permit; nothing was sent"
        )));
    };
    let parsed = Origin::parse(&req.url);
    let path = parsed
        .as_ref()
        .map_or(req.url.as_str(), |(_, path)| path_only(path));
    let on_credential_origin = match (&parsed, req.auth) {
        (Some((origin, _)), Some(auth)) => origin == auth.origin(),
        // No credential: check 4 refuses it, naming `none`.
        (Some(_), None) => true,
        (None, _) => false,
    };
    let path_and_query = parsed
        .as_ref()
        .map_or(req.url.as_str(), |(_, path)| path.as_str());
    let covered = match (req.atomic_refs, req.repo_create) {
        (Some(scope), None) => scope.covers(
            permit,
            req.method,
            parsed.as_ref().map(|(origin, _)| origin),
            parsed
                .as_ref()
                .map_or(req.url.as_str(), |(_, path)| path.as_str()),
            &req.body,
        ),
        (None, Some(scope)) => scope.covers(
            permit,
            req.method,
            parsed.as_ref().map(|(origin, _)| origin),
            path_and_query,
            &req.body,
        ),
        (None, None) => permit.covers(path),
        (Some(_), Some(_)) => false,
    };
    if !on_credential_origin || !covered {
        let shown = match (&parsed, on_credential_origin) {
            (Some((origin, _)), false) => format!("{}{path}", origin.base()),
            _ => path.to_string(),
        };
        return Some(refused(format!(
            "the permit covers {}, not {shown}",
            target_text(permit.target())
        )));
    }
    // No credential is refused by name, never by the sentinel: a profile may be named `none`.
    match req.auth {
        None => Some(refused(format!(
            "the permit is {}'s, the credential none's",
            permit.profile()
        ))),
        Some(auth) if auth.profile() != permit.profile() => Some(refused(format!(
            "the permit is {}'s, the credential {}'s",
            permit.profile(),
            auth.profile()
        ))),
        Some(_) => None,
    }
}

fn guard(req: &WireRequest<'_>) -> Option<WireError> {
    if !req.method.is_read() {
        return write_guard(req);
    }
    let auth = req.auth?;
    let target = Origin::parse(&req.url).map(|(origin, _)| origin);
    if target.as_ref() == Some(auth.origin()) {
        return None;
    }
    let to = target.map_or_else(|| "an unparsable URL".to_string(), |o| o.base());
    Some(refused(format!(
        "a credential built for {} is never sent to {to}",
        auth.origin().base()
    )))
}

const fn http_method(method: Method) -> aterm_http::Method {
    match method {
        Method::Get => aterm_http::Method::Get,
        Method::Head => aterm_http::Method::Head,
        Method::Post => aterm_http::Method::Post,
        Method::Patch => aterm_http::Method::Patch,
        Method::Put => aterm_http::Method::Put,
        Method::Delete => aterm_http::Method::Delete,
    }
}

fn names_a_proxy_variable(message: &str) -> bool {
    message.split_once(' ').is_some_and(|(name, _)| {
        name.len() >= 6 && name.as_bytes()[name.len() - 6..].eq_ignore_ascii_case(b"_proxy")
    })
}

fn kind_of(phase: Phase, error: &aterm_http::Error) -> WireKind {
    use std::io::ErrorKind as K;
    match error {
        aterm_http::Error::Protocol(m) if phase == Phase::NotSent && m.starts_with("proxy ") => {
            WireKind::Proxy
        }
        aterm_http::Error::Invalid(m) if phase == Phase::NotSent && names_a_proxy_variable(m) => {
            WireKind::Proxy
        }
        aterm_http::Error::Invalid(_) => WireKind::Local,
        aterm_http::Error::Protocol(_) => WireKind::Protocol,
        aterm_http::Error::TooLarge { .. } => WireKind::TooLarge,
        aterm_http::Error::Io(e) if phase == Phase::NotSent => {
            let message = e.to_string();
            if message.starts_with("proxy") {
                WireKind::Proxy
            } else if message.starts_with("dns:") {
                WireKind::Dns
            } else if message.contains("invalid peer certificate") {
                WireKind::Trust
            } else {
                match e.kind() {
                    K::InvalidData => WireKind::Tls,
                    K::TimedOut => WireKind::Timeout,
                    K::PermissionDenied => WireKind::Revoked,
                    _ => WireKind::Connect,
                }
            }
        }
        aterm_http::Error::Io(e) => wire_kind_of(e),
    }
}

const fn phase_of(phase: aterm_http::Phase) -> Phase {
    match phase {
        aterm_http::Phase::NotSent => Phase::NotSent,
        aterm_http::Phase::Sent => Phase::Sent,
        aterm_http::Phase::Answered => Phase::Answered,
    }
}

impl crate::wire::Transport for HttpTransport {
    fn round_trip(&self, req: WireRequest<'_>) -> Result<WireResponse, WireError> {
        if let Some(refusal) = guard(&req) {
            return Err(refusal);
        }
        let WireRequest {
            method,
            url,
            headers,
            auth,
            body,
            limit,
            deadlines,
            // The guard above judged the permit; nothing past it reads the permit, the
            // body's digest (a tape's) or the profile it speaks as (the credential's own).
            permit: _,
            body_digest: _,
            as_profile: _,
            atomic_refs: _,
            repo_create: _,
        } = req;
        let mut builder = self.client.request(http_method(method), &url);
        for (name, value) in &headers {
            builder = builder.header(name, value);
        }
        if let Some(auth) = auth {
            let (name, value) = auth.parts();
            builder = builder.secret_header(name, Secret::new(value.expose()));
        }
        let builder = builder
            .limit(usize::try_from(limit).unwrap_or(usize::MAX))
            .idle(deadlines.idle)
            .backstop(deadlines.backstop)
            .connect_within(deadlines.connect);
        let body = match body {
            WireBody::Empty => Body::Empty,
            WireBody::Bytes(bytes) => Body::Bytes(bytes),
            WireBody::Reader { len, reader } => Body::Reader { len, reader },
        };
        match builder.exchange_with_env(body, &EnvAdapter(&*self.env)) {
            Ok(exchange) => Ok(WireResponse {
                status: exchange.status(),
                headers: exchange
                    .headers()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
                body: Box::new(exchange.into_body()),
            }),
            Err(SendError { phase, error }) => {
                let phase = phase_of(phase);
                Err(WireError {
                    phase,
                    kind: kind_of(phase, &error),
                    detail: error.to_string(),
                })
            }
        }
    }
}

impl super::sealed::Sealed for HttpTransport {}

#[cfg(test)]
#[path = "permit_tests.rs"]
mod permit_tests;

#[cfg(test)]
mod tests {
    use std::io::{Error as IoError, ErrorKind as K};

    use super::{Phase, WireKind, kind_of};

    fn io(kind: K, text: &str) -> aterm_http::Error {
        aterm_http::Error::Io(IoError::new(kind, text))
    }

    #[test]
    fn every_row_of_the_transport_table() {
        let rows = [
            (
                Phase::NotSent,
                aterm_http::Error::Invalid("x".into()),
                WireKind::Local,
            ),
            (
                Phase::Answered,
                aterm_http::Error::Protocol("x".into()),
                WireKind::Protocol,
            ),
            (
                Phase::Sent,
                aterm_http::Error::TooLarge { limit: 1 },
                WireKind::TooLarge,
            ),
            (Phase::NotSent, io(K::Other, "proxy x"), WireKind::Proxy),
            (
                Phase::NotSent,
                aterm_http::Error::Protocol("proxy refused CONNECT with status 407".into()),
                WireKind::Proxy,
            ),
            (
                Phase::NotSent,
                aterm_http::Error::Protocol("proxy tunnel lost its socket".into()),
                WireKind::Proxy,
            ),
            (
                Phase::NotSent,
                aterm_http::Error::Invalid(
                    "HTTPS_PROXY is set but is not a usable http:// proxy URL".into(),
                ),
                WireKind::Proxy,
            ),
            (
                Phase::NotSent,
                aterm_http::Error::Invalid(
                    "https_proxy names an https:// proxy, which this client does not support; use an http:// proxy URL".into(),
                ),
                WireKind::Proxy,
            ),
            (
                Phase::NotSent,
                aterm_http::Error::Invalid("invalid header name: \"a b\"".into()),
                WireKind::Local,
            ),
            (
                Phase::NotSent,
                aterm_http::Error::Protocol("empty status line".into()),
                WireKind::Protocol,
            ),
            (
                Phase::Answered,
                aterm_http::Error::Protocol("proxy x".into()),
                WireKind::Protocol,
            ),
            (
                Phase::NotSent,
                io(K::TimedOut, "dns: no answer within 100ms"),
                WireKind::Dns,
            ),
            (
                Phase::NotSent,
                io(K::NotFound, "dns: example.invalid: x"),
                WireKind::Dns,
            ),
            (
                Phase::NotSent,
                io(
                    K::InvalidData,
                    "TLS handshake failed: invalid peer certificate: x",
                ),
                WireKind::Trust,
            ),
            (Phase::NotSent, io(K::InvalidData, "x"), WireKind::Tls),
            (
                Phase::NotSent,
                io(K::TimedOut, "no bytes for 200ms"),
                WireKind::Timeout,
            ),
            (
                Phase::NotSent,
                io(K::PermissionDenied, "x"),
                WireKind::Revoked,
            ),
            (
                Phase::NotSent,
                io(K::ConnectionRefused, "x"),
                WireKind::Connect,
            ),
            (Phase::NotSent, io(K::Other, "x"), WireKind::Connect),
            (Phase::Sent, io(K::UnexpectedEof, "x"), WireKind::Truncated),
            (
                Phase::Sent,
                io(K::TimedOut, "no bytes for 200ms"),
                WireKind::Idle,
            ),
            (Phase::Answered, io(K::InvalidData, "x"), WireKind::Protocol),
            (Phase::Sent, io(K::ConnectionReset, "x"), WireKind::Reset),
        ];
        for (phase, error, kind) in rows {
            assert_eq!(kind_of(phase, &error), kind, "{phase:?} {error:?}");
        }
    }
}
