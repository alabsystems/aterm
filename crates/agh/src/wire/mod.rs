// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The wire (design §5, §11.4): methods, phases, origins, endpoints, deadlines and the
//! transport trait. Exactly one file renders the credential header: `door.rs`.

pub mod door;
pub mod hop;
pub mod http;
#[cfg(any(test, debug_assertions))]
pub mod scripted;

use std::io::Read;
use std::time::Duration;

/// An HTTP method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`.
    Get,
    /// `HEAD`.
    Head,
    /// `POST`.
    Post,
    /// `PATCH`.
    Patch,
    /// `PUT`.
    Put,
    /// `DELETE`.
    Delete,
}

impl Method {
    /// `GET`, `HEAD`, `POST`, `PATCH`, `PUT`, `DELETE`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }

    /// GET and HEAD.
    #[must_use]
    pub const fn is_read(self) -> bool {
        matches!(self, Method::Get | Method::Head)
    }
}

/// How far a failed exchange got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing was sent.
    NotSent,
    /// The request was sent and no answer was read.
    Sent,
    /// An answer was being read.
    Answered,
}

/// What failed on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WireKind {
    /// The name did not resolve.
    Dns,
    /// The connection was refused or failed.
    Connect,
    /// The proxy failed.
    Proxy,
    /// The TLS handshake failed.
    Tls,
    /// The certificate was not trusted.
    Trust,
    /// The request's deadline passed.
    Timeout,
    /// No bytes arrived within the idle deadline.
    Idle,
    /// The connection was reset.
    Reset,
    /// The answer ended early.
    Truncated,
    /// The answer was longer than its limit.
    TooLarge,
    /// The answer broke HTTP.
    Protocol,
    /// The authority to connect was revoked.
    Revoked,
    /// A local failure.
    Local,
}

impl WireKind {
    /// `dns`, `connect`, `proxy`, `tls`, `trust`, `timeout`, `idle`, `reset`, `truncated`,
    /// `too_large`, `protocol`, `revoked`, `local`.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            WireKind::Dns => "dns",
            WireKind::Connect => "connect",
            WireKind::Proxy => "proxy",
            WireKind::Tls => "tls",
            WireKind::Trust => "trust",
            WireKind::Timeout => "timeout",
            WireKind::Idle => "idle",
            WireKind::Reset => "reset",
            WireKind::Truncated => "truncated",
            WireKind::TooLarge => "too_large",
            WireKind::Protocol => "protocol",
            WireKind::Revoked => "revoked",
            WireKind::Local => "local",
        }
    }
}

/// A failed exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireError {
    /// How far it got.
    pub phase: Phase,
    /// What failed.
    pub kind: WireKind,
    /// The failure's text.
    pub detail: String,
}

/// The kind of an I/O error met while sending or reading, by the first row that matches:
/// - a message starting `dns:` → Dns (aterm-http's own text, the same on every platform, so a
///   resolver timeout is Dns and not Timeout);
/// - `UnexpectedEof` → Truncated;
/// - `InvalidData` → Protocol;
/// - `TimedOut` or `WouldBlock` → Idle when the message contains `no bytes for`, else Timeout;
/// - `ConnectionReset`, `ConnectionAborted` or `BrokenPipe` → Reset;
/// - `PermissionDenied` → Revoked;
/// - `ConnectionRefused` → Connect;
/// - an inner `aterm_http::Error::TooLarge` (by `get_ref` and downcast) → TooLarge;
/// - anything else → Local.
#[must_use]
pub fn wire_kind_of(e: &std::io::Error) -> WireKind {
    use std::io::ErrorKind as K;
    let message = e.to_string();
    if message.starts_with("dns:") {
        return WireKind::Dns;
    }
    match e.kind() {
        K::UnexpectedEof => WireKind::Truncated,
        K::InvalidData => WireKind::Protocol,
        K::TimedOut | K::WouldBlock => {
            if message.contains("no bytes for") {
                WireKind::Idle
            } else {
                WireKind::Timeout
            }
        }
        K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe => WireKind::Reset,
        K::PermissionDenied => WireKind::Revoked,
        K::ConnectionRefused => WireKind::Connect,
        _ => {
            let too_large = e
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<aterm_http::Error>())
                .is_some_and(|inner| matches!(inner, aterm_http::Error::TooLarge { .. }));
            if too_large {
                WireKind::TooLarge
            } else {
                WireKind::Local
            }
        }
    }
}

/// The role an origin plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginRole {
    /// The REST API.
    Api,
    /// Release-asset uploads.
    Uploads,
    /// The web origin.
    Web,
    /// A download CDN host.
    AssetCdn,
}

/// A scheme, a lowercased host and a port. Opaque: built by `https`, `parse` and the
/// `Endpoints` constructors, and read through its accessors.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub(crate) scheme: &'static str,
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl Origin {
    /// `https`, the host lowercased, port 443.
    #[must_use]
    pub fn https(host: &str) -> Origin {
        Origin {
            scheme: "https",
            host: host.to_ascii_lowercase(),
            port: 443,
        }
    }

    /// `http` on a loopback port: agh-fake's origins.
    #[cfg(any(test, debug_assertions))]
    fn loopback(port: u16) -> Origin {
        Origin {
            scheme: "http",
            host: "127.0.0.1".to_string(),
            port,
        }
    }

    /// `http` or `https`.
    #[must_use]
    pub fn scheme(&self) -> &'static str {
        self.scheme
    }

    /// The host, lowercased.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port, the scheme's default filled in.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `scheme://host`, with `:port` only when it is not the scheme's default.
    #[must_use]
    pub fn base(&self) -> String {
        if self.port == default_port(self.scheme) {
            format!("{}://{}", self.scheme, self.host)
        } else {
            format!("{}://{}:{}", self.scheme, self.host, self.port)
        }
    }

    /// An `http` or `https` URL: its origin (host lowercased, default ports filled) and its
    /// path and query (`/` when empty; a `#fragment` dropped). `None` for any other scheme,
    /// for userinfo (`@` before the path), for a bracketed host, or for a bad port.
    #[must_use]
    pub fn parse(url: &str) -> Option<(Origin, String)> {
        let (scheme, rest) = if let Some(rest) = strip_prefix_ci(url, "https://") {
            ("https", rest)
        } else {
            ("http", strip_prefix_ci(url, "http://")?)
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(split);
        if authority.contains('@') || authority.starts_with('[') || authority.is_empty() {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, crate::path::digits::<u16>(port)?),
            None => (authority, default_port(scheme)),
        };
        if host.is_empty() {
            return None;
        }
        let path = if tail.is_empty() {
            "/".to_string()
        } else if tail.starts_with('?') {
            format!("/{tail}")
        } else {
            tail.to_string()
        };
        Some((
            Origin {
                scheme,
                host: host.to_ascii_lowercase(),
                port,
            },
            path,
        ))
    }
}

/// `text` without `prefix`, matched ASCII case-insensitively.
pub(crate) fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

/// The scheme's default port: 443 for `https`, else (`http`) 80.
fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

/// The origins agh speaks to. Opaque: its fields are crate-private and it has no other
/// constructor, so outside agh an `Endpoints` is `github_com()` or, in a test or debug build,
/// `loopback(..)`, and no caller names a host of its own (design §6.6). `#[non_exhaustive]`
/// alone would not do: it stops a struct literal, not `e.api = Origin::https("mirror.example")`.
/// The credential door checks GitHub's own hosts besides, so an `Endpoints` built in the crate
/// cannot steer a token either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    pub(crate) api: Origin,
    pub(crate) uploads: Origin,
    pub(crate) web: Origin,
    pub(crate) asset_hosts: Vec<Origin>,
}

impl Endpoints {
    /// `https://api.github.com`, `https://uploads.github.com`, `https://github.com`, and the
    /// CDN hosts `objects.githubusercontent.com`, `release-assets.githubusercontent.com` and
    /// `github-releases.githubusercontent.com` (https, 443).
    #[must_use]
    pub fn github_com() -> Endpoints {
        Endpoints {
            api: Origin::https("api.github.com"),
            uploads: Origin::https("uploads.github.com"),
            web: Origin::https("github.com"),
            asset_hosts: vec![
                Origin::https("objects.githubusercontent.com"),
                Origin::https("release-assets.githubusercontent.com"),
                Origin::https("github-releases.githubusercontent.com"),
            ],
        }
    }

    /// Four `http://127.0.0.1:<port>` origins; the CDN list is the one `cdn` origin.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn loopback(api: u16, uploads: u16, web: u16, cdn: u16) -> Endpoints {
        Endpoints {
            api: Origin::loopback(api),
            uploads: Origin::loopback(uploads),
            web: Origin::loopback(web),
            asset_hosts: vec![Origin::loopback(cdn)],
        }
    }

    /// The REST origin, and one of the two a credential may go to.
    #[must_use]
    pub fn api(&self) -> &Origin {
        &self.api
    }

    /// The release-asset upload origin, the other one.
    #[must_use]
    pub fn uploads(&self) -> &Origin {
        &self.uploads
    }

    /// The web origin: anonymous only.
    #[must_use]
    pub fn web(&self) -> &Origin {
        &self.web
    }

    /// The CDN hosts a hop may reach, in order; never empty.
    #[must_use]
    pub fn asset_hosts(&self) -> &[Origin] {
        &self.asset_hosts
    }

    /// The role's origin (`AssetCdn`: the first CDN host).
    #[must_use]
    pub fn origin(&self, role: OriginRole) -> &Origin {
        match role {
            OriginRole::Api => &self.api,
            OriginRole::Uploads => &self.uploads,
            OriginRole::Web => &self.web,
            OriginRole::AssetCdn => self.asset_hosts.first().unwrap_or(&self.web),
        }
    }

    /// The role of an origin equal (scheme, host and port) to one of these.
    #[must_use]
    pub fn role_of(&self, origin: &Origin) -> Option<OriginRole> {
        if *origin == self.api {
            Some(OriginRole::Api)
        } else if *origin == self.uploads {
            Some(OriginRole::Uploads)
        } else if *origin == self.web {
            Some(OriginRole::Web)
        } else if self.asset_hosts.contains(origin) {
            Some(OriginRole::AssetCdn)
        } else {
            None
        }
    }

    /// `base() + path_and_query` for the role's origin (`AssetCdn`: the first CDN host). A
    /// `path_and_query` that does not start with `/` gets one, so no path can extend the
    /// authority (`.evil.example/x`, `:8443/x`): the URL is always on the origin.
    #[must_use]
    pub fn url(&self, role: OriginRole, path_and_query: &str) -> String {
        let slash = if path_and_query.starts_with('/') {
            ""
        } else {
            "/"
        };
        format!("{}{slash}{path_and_query}", self.origin(role).base())
    }
}

/// One request's time limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deadlines {
    /// The connect deadline.
    pub connect: Duration,
    /// The longest wait for the next byte.
    pub idle: Duration,
    /// The whole request's deadline.
    pub backstop: Duration,
}

impl Deadlines {
    /// A JSON call: connect 10 s, idle 60 s, backstop 60 s.
    pub const JSON: Deadlines = Deadlines {
        connect: Duration::from_secs(10),
        idle: Duration::from_secs(60),
        backstop: Duration::from_secs(60),
    };

    /// A transfer of `bytes`: connect 10 s, idle 60 s, backstop max(600 s, bytes / 4096 s).
    #[must_use]
    pub fn for_transfer(bytes: u64) -> Deadlines {
        Deadlines {
            backstop: Duration::from_secs((bytes / 4096).max(600)),
            ..Deadlines::JSON
        }
    }

    /// The same deadlines with another backstop.
    #[must_use]
    pub fn with_backstop(self, backstop: Duration) -> Deadlines {
        Deadlines { backstop, ..self }
    }
}

/// A request body.
pub enum WireBody<'a> {
    /// No body.
    Empty,
    /// These bytes.
    Bytes(&'a [u8]),
    /// A reader of `len` bytes.
    Reader {
        /// The body's length.
        len: u64,
        /// Where the bytes come from.
        reader: &'a mut (dyn Read + Send),
    },
}

/// One request, as the client hands it to a transport. `#[non_exhaustive]`: outside agh it is
/// built with `WireRequest::new` and its fields set after, so M2 adds a crate-private `permit`
/// field additively, which `HttpTransport` checks before it sends a write (design §10.1).
#[non_exhaustive]
pub struct WireRequest<'a> {
    /// The method.
    pub method: Method,
    /// The absolute URL.
    pub url: String,
    /// Every header but the credential, in send order.
    pub headers: Vec<(String, String)>,
    /// The credential header. Only `door::AuthHeader::for_origin` (or its test twin) makes one,
    /// and `HttpTransport` sends it only to the origin it was made for (plan §8, step 0).
    pub auth: Option<&'a door::AuthHeader>,
    /// The body.
    pub body: WireBody<'a>,
    /// The most body bytes the answer may carry.
    pub limit: u64,
    /// The time limits.
    pub deadlines: Deadlines,
}

impl<'a> WireRequest<'a> {
    /// `method` to `url` with `headers`, `limit` and `deadlines`: no credential, an empty body.
    pub fn new(
        method: Method,
        url: impl Into<String>,
        headers: Vec<(String, String)>,
        limit: u64,
        deadlines: Deadlines,
    ) -> WireRequest<'a> {
        WireRequest {
            method,
            url: url.into(),
            headers,
            auth: None,
            body: WireBody::Empty,
            limit,
            deadlines,
        }
    }
}

/// An answer whose body has not been read.
pub struct WireResponse {
    /// The status.
    pub status: u16,
    /// The headers, in received order.
    pub headers: Vec<(String, String)>,
    /// The body, unread.
    pub body: Box<dyn Read + Send>,
}

/// The first header in `headers` so named, ASCII case-insensitively: the one lookup every
/// reader of a head shares.
pub(crate) fn header_in<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// At most `cap + 1` bytes of `body`, so a caller can tell a body longer than `cap` from one
/// that fits. An I/O error → `WireError { Answered, wire_kind_of(&e), e }`.
pub(crate) fn read_capped(body: impl Read, cap: u64) -> Result<Vec<u8>, WireError> {
    let mut bytes = Vec::new();
    if let Err(e) = body.take(cap.saturating_add(1)).read_to_end(&mut bytes) {
        return Err(WireError {
            phase: Phase::Answered,
            kind: wire_kind_of(&e),
            detail: e.to_string(),
        });
    }
    Ok(bytes)
}

/// `path` without its query.
pub(crate) fn path_only(path: &str) -> &str {
    path.split('?').next().unwrap_or_default()
}

impl WireResponse {
    /// The first header so named, ASCII case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header_in(&self.headers, name)
    }

    /// Every header so named, in order.
    pub fn header_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body, whole. Over `limit` → `WireError { Answered, TooLarge, "the answer is longer
    /// than {limit} bytes" }`; an I/O error → `WireError { Answered, wire_kind_of(&e), e }`.
    ///
    /// # Errors
    ///
    /// As above.
    pub fn read_body(&mut self, limit: u64) -> Result<Vec<u8>, WireError> {
        let body = read_capped(&mut self.body, limit)?;
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > limit {
            return Err(WireError {
                phase: Phase::Answered,
                kind: WireKind::TooLarge,
                detail: format!("the answer is longer than {limit} bytes"),
            });
        }
        Ok(body)
    }
}

impl std::fmt::Debug for WireResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// Sends one request. `HttpTransport` in production, `ScriptedTransport` in unit tests. Only
/// `fx/` calls `round_trip` (census `agh-fx`): the client reaches it through `Fx::gh` and
/// `Fx::gh_stream`, so a tape sees every round trip. In M1 the client asks only for GET and
/// HEAD (its pipeline's step 0). `HttpTransport` refuses any other method, and any credential
/// on a URL of another origin than the one the door built it for.
///
/// Sealed: its supertrait `sealed::Sealed` is a public trait in a private module (the tree's
/// pattern: `aterm-scrollback`'s `access.rs`, `aterm-provenance`'s `origin.rs`), so code outside
/// agh can hold a `dyn Transport` and call it but never implement it. `HttpTransport` and
/// `ScriptedTransport` are the only transports, and a header the pipeline built is handed to
/// no code but agh's own (design §10.1).
pub trait Transport: Send + Sync + sealed::Sealed {
    /// One exchange.
    ///
    /// # Errors
    ///
    /// Why the exchange failed, and how far it got.
    fn round_trip(&self, req: WireRequest<'_>) -> Result<WireResponse, WireError>;
}

mod sealed {
    /// Implemented by `HttpTransport` and `ScriptedTransport` alone (`wire/http.rs`,
    /// `wire/scripted.rs`, the children of this module's parent).
    pub trait Sealed {}
}
