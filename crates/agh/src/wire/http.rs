// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The production transport: aterm-http's client (design §5), one request per connection.

use std::ffi::OsString;
use std::sync::Arc;

use aterm_http::{Body, Client, ProxyMode, SendError, Trust};
use aterm_uds::secret::Secret;

use crate::identity::EnvSource;
use crate::wire::{
    Deadlines, Method, Origin, Phase, WireBody, WireError, WireKind, WireRequest, WireResponse,
    wire_kind_of,
};

/// Sends through aterm-http with the platform trust store and the environment's proxy.
pub struct HttpTransport {
    client: Client,
    env: Arc<dyn EnvSource + Send + Sync>,
}

impl HttpTransport {
    /// `Client::new(Trust::PlatformVerifier, ProxyMode::Environment, Deadlines::JSON.backstop)`;
    /// proxy variables are read from `env`.
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

    /// Loopback tests: `Trust::Roots(Vec::new())` (plain HTTP never reaches TLS, and an https
    /// URL fails closed) and `ProxyMode::Direct`.
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

/// agh's environment source, handed to aterm-http's generic `exchange_with_env`.
struct EnvAdapter<'a>(&'a (dyn EnvSource + Send + Sync));

impl EnvSource for EnvAdapter<'_> {
    fn var_os(&self, name: &str) -> Option<OsString> {
        self.0.var_os(name)
    }
}

/// A refusal before any connection: nothing is sent.
fn refused(detail: String) -> WireError {
    WireError {
        phase: Phase::NotSent,
        kind: WireKind::Local,
        detail,
    }
}

/// Step 0: the method, then the credential's origin (plan §8). `None` when the request may go.
fn guard(req: &WireRequest<'_>) -> Option<WireError> {
    if !req.method.is_read() {
        return Some(refused(format!(
            "agh M1 sends only GET and HEAD; {} is refused",
            req.method.as_str()
        )));
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

/// agh's method as aterm-http's.
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

/// Whether aterm-http's `Invalid` text is its refusal of a proxy variable: it starts with the
/// variable's name (`HTTPS_PROXY`, `http_proxy`, `ALL_PROXY`, …), then a space.
fn names_a_proxy_variable(message: &str) -> bool {
    message
        .split_once(' ')
        .is_some_and(|(name, _)| name.to_ascii_lowercase().ends_with("_proxy"))
}

/// The kind of a failed exchange, by the first row of plan §8's table that matches, after two rows
/// beside it: a proxy failure aterm-http reports as `Protocol` (the CONNECT refused) or as
/// `Invalid` (an unusable proxy variable), at NotSent, is Proxy (design §7.2's wire row), where
/// the table's own `Io … proxy` row matches no text aterm-http writes.
fn kind_of(phase: Phase, error: &aterm_http::Error) -> WireKind {
    use std::io::ErrorKind as K;
    match error {
        // aterm-http names the proxy in these two: a CONNECT the proxy refused or whose
        // socket it lost (`proxy refused CONNECT …`, `proxy tunnel …`), and a proxy variable
        // it cannot use (`HTTPS_PROXY is set but …`, `https_proxy names an https:// proxy …`).
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

/// aterm-http's phase as agh's.
const fn phase_of(phase: aterm_http::Phase) -> Phase {
    match phase {
        aterm_http::Phase::NotSent => Phase::NotSent,
        aterm_http::Phase::Sent => Phase::Sent,
        aterm_http::Phase::Answered => Phase::Answered,
    }
}

impl crate::wire::Transport for HttpTransport {
    /// plan §8's steps: the two guards, then one aterm-http exchange whose body is left unread.
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
            // aterm-http's own proxy texts (its `connect_tunnel` and `parse_proxy`).
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
            // Not a proxy's: the generic rows.
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
