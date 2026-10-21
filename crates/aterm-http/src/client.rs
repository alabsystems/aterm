// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HTTP/1.1 request building and response parsing.
//!
//! Scope was set by the caller it was written for (the title-summary worker,
//! retired 2026-10-15): one request per connection, a known-length body,
//! `Connection: close`, and NO redirect following (an API endpoint that
//! redirects is a misconfiguration, and chasing it is how a bearer token
//! reaches a host the caller never named).
//! There is no connection pool and no HTTP/2, because a caller making one
//! bounded call at a time needs neither.
//!
//! * Header names and values containing CR, LF or NUL are rejected, so a
//!   configured value cannot inject a second request; the request target is
//!   never re-encoded ([`crate::uri`]).
//! * The body is bounded by [`RequestBuilder::limit`] and the head by
//!   [`MAX_HEADER_BYTES`]; exceeding either is an error, not a truncation.
//! * Every response whose end is ambiguous is refused (`decide_framing`).
//! * Interim 1xx responses are read past, on CONNECT too (`read_head`).
//! * An unusable endpoint is refused without echoing it (userinfo carries a
//!   credential); an unusable proxy setting fails the request, naming its
//!   variable, and never falls back to a direct connection ([`crate::proxy`]).

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::proxy::{self, EnvSource, ProcessEnv, ProxyMode, ProxyTarget};
use crate::stream::{Connect, Deadline, Guard, Stream, TcpConnector};
use crate::tls::{self, Trust};
use crate::uri::{Scheme, Uri};

/// Cap on the response header block, headers and status line together.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;
/// Cap on a single header line.
const MAX_HEADER_LINE: usize = 8 * 1024;
/// Cap on the number of header lines.
const MAX_HEADER_COUNT: usize = 128;

/// A failure anywhere in a request.
#[derive(Debug)]
pub enum Error {
    /// The endpoint, a header, or the configuration was not usable.
    Invalid(String),
    /// Transport failure — connect, TLS, read or write.
    Io(std::io::Error),
    /// The response did not parse as HTTP/1.x.
    Protocol(String),
    /// The response body exceeded the configured limit.
    TooLarge {
        /// The limit that was exceeded, in bytes.
        limit: usize,
    },
}

impl Error {
    /// Whether this is the revoked-authority error, as opposed to a network
    /// fault. Callers surface the two differently.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "{message}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Protocol(message) => write!(formatter, "malformed HTTP response: {message}"),
            Self::TooLarge { limit } => {
                write!(formatter, "response exceeded the {limit}-byte limit")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// One lazily-built TLS configuration, shared by every clone of a [`Client`].
/// The error is cached too: a broken trust configuration must fail consistently
/// without rescanning or reparsing the same trust store on every request.
#[derive(Default)]
struct TlsConfigCache {
    value: OnceLock<Result<Arc<rustls::ClientConfig>, String>>,
    #[cfg(test)]
    builds: std::sync::atomic::AtomicUsize,
}

impl TlsConfigCache {
    fn get(&self, trust: &Trust) -> Result<Arc<rustls::ClientConfig>, String> {
        self.value
            .get_or_init(|| {
                #[cfg(test)]
                self.builds
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tls::client_config(trust)
            })
            .clone()
    }

    #[cfg(test)]
    fn builds(&self) -> usize {
        self.builds.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Client configuration shared by every request it issues.
///
/// Clones share one lazy TLS configuration, built at the first HTTPS request
/// and reused after (on non-Apple Unix that is when `SSL_CERT_FILE`,
/// `SSL_CERT_DIR` and the on-disk store are read; a new client adopts later
/// trust-store changes). Proxy variables are resolved afresh per request.
#[derive(Clone)]
pub struct Client {
    trust: Arc<Trust>,
    proxy_mode: ProxyMode,
    timeout: Duration,
    connector: Arc<dyn Connect>,
    tls_config: Arc<TlsConfigCache>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("trust", &self.trust)
            .field("proxy_mode", &self.proxy_mode)
            .field("timeout", &self.timeout)
            .field("connector", &self.connector)
            .field(
                "tls_config_initialized",
                &self.tls_config.value.get().is_some(),
            )
            .finish()
    }
}

impl Client {
    /// A client with the given trust model, proxy policy and global timeout.
    #[must_use]
    pub fn new(trust: Trust, proxy_mode: ProxyMode, timeout: Duration) -> Self {
        Self::with_shared_trust(Arc::new(trust), proxy_mode, timeout)
    }

    /// A client that shares an already-owned trust model, so a cache keyed by
    /// custom CA roots and its clients share one DER allocation.
    #[must_use]
    pub fn with_shared_trust(trust: Arc<Trust>, proxy_mode: ProxyMode, timeout: Duration) -> Self {
        Self {
            trust,
            proxy_mode,
            timeout,
            connector: Arc::new(TcpConnector),
            tls_config: Arc::new(TlsConfigCache::default()),
        }
    }

    /// Replace the TCP connector — the seam an attesting, socket-pinned
    /// connector uses.
    #[must_use]
    pub fn with_connector(mut self, connector: Arc<dyn Connect>) -> Self {
        self.connector = connector;
        self
    }

    /// Begin a POST to `endpoint`.
    #[must_use]
    pub fn post(&self, endpoint: &str) -> RequestBuilder<'_> {
        self.request("POST", endpoint)
    }

    /// Begin a PUT to `endpoint`. Its body is framed by `Content-Length`, as a
    /// POST's is.
    #[must_use]
    pub fn put(&self, endpoint: &str) -> RequestBuilder<'_> {
        self.request("PUT", endpoint)
    }

    /// Begin a GET to `endpoint`. Send it with an empty body: the head then
    /// carries no `Content-Length` (RFC 9110 §8.6: a GET anticipates no
    /// content).
    #[must_use]
    pub fn get(&self, endpoint: &str) -> RequestBuilder<'_> {
        self.request("GET", endpoint)
    }

    fn request(&self, method: &'static str, endpoint: &str) -> RequestBuilder<'_> {
        RequestBuilder {
            client: self,
            method,
            endpoint: endpoint.to_owned(),
            headers: Vec::new(),
            limit: usize::MAX,
            guard: None,
            error: None,
        }
    }
}

/// A request under construction.
pub struct RequestBuilder<'a> {
    client: &'a Client,
    method: &'static str,
    endpoint: String,
    headers: Vec<(String, String)>,
    limit: usize,
    guard: Option<Arc<dyn Guard>>,
    /// First construction error, surfaced at `send` so the builder stays
    /// chainable.
    error: Option<Error>,
}

impl RequestBuilder<'_> {
    /// Add a header. A name or value containing CR, LF or NUL is rejected.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let Err(error) = validate_header(name, value) {
            self.error.get_or_insert(error);
            return self;
        }
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Bound the response body. Exceeding it is an error, not a truncation.
    #[must_use]
    pub const fn limit(mut self, bytes: usize) -> Self {
        self.limit = bytes;
        self
    }

    /// Attach the revocable authority checked at every I/O step.
    #[must_use]
    pub fn guard(mut self, guard: Arc<dyn Guard>) -> Self {
        self.guard = Some(guard);
        self
    }

    /// Send `body` and read the whole response.
    ///
    /// # Errors
    ///
    /// Any configuration, transport, or protocol failure, or a body over the
    /// configured limit.
    pub fn send(self, body: &[u8]) -> Result<Response, Error> {
        self.send_with_env(body, &ProcessEnv)
    }

    /// [`Self::send`] against an injected environment source (tests).
    ///
    /// # Errors
    ///
    /// As [`Self::send`].
    pub fn send_with_env<E: EnvSource>(self, body: &[u8], env: &E) -> Result<Response, Error> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let target = Uri::parse(&self.endpoint)
            // The raw endpoint is NEVER echoed. `uri::parse` rejects userinfo
            // rather than stripping it, precisely so a credential cannot ride
            // along — and a caller may print this error to stderr, so
            // interpolating the string that just failed to parse would print
            // the password of the one endpoint shape the policy exists to
            // refuse. The caller already holds the configured value to
            // correlate against.
            .ok_or_else(|| {
                Error::Invalid("endpoint is not a usable absolute http(s) URL".to_owned())
            })?;
        let deadline = Deadline::after(self.client.timeout);
        let guard: Arc<dyn Guard> = self
            .guard
            .unwrap_or_else(|| Arc::new(crate::stream::AlwaysAuthorized));
        let via = proxy::resolve(self.client.proxy_mode, &target, env)?;

        let mut stream = self.client.open(&target, via.as_ref(), &guard, deadline)?;
        let head = render_request(
            self.method,
            &target,
            via.as_ref(),
            &self.headers,
            body.len(),
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(body)?;
        stream.flush()?;
        read_response(stream, self.limit)
    }
}

impl Client {
    /// Establish the byte stream for `target`, tunnelling through `via` when a
    /// proxy applies.
    fn open(
        &self,
        target: &Uri,
        via: Option<&ProxyTarget>,
        guard: &Arc<dyn Guard>,
        deadline: Deadline,
    ) -> Result<Stream, Error> {
        // A `ProxyTarget` is always a plain `http://` proxy (`proxy::resolve`
        // refuses an `https://` one), so plaintext to it is what it speaks.
        let (host, port) = match via {
            Some(p) => (p.uri().host().to_owned(), p.uri().port()),
            None => (target.host().to_owned(), target.port()),
        };
        let tcp = self.connector.connect(&host, port, deadline)?;
        match (target.scheme(), via) {
            // Plain HTTP: the proxy (if any) is spoken to in absolute-form; no
            // tunnel is needed.
            (Scheme::Http, _) => Ok(Stream::plain(tcp, Arc::clone(guard), deadline)),
            // HTTPS through a proxy: CONNECT first, then handshake against the
            // origin name so the proxy cannot substitute its own certificate.
            (Scheme::Https, Some(proxy)) => {
                let mut plain = Stream::plain(tcp, Arc::clone(guard), deadline);
                connect_tunnel(&mut plain, target, proxy.basic_auth())?;
                let tcp = plain
                    .into_tcp()
                    .ok_or_else(|| Error::Protocol("proxy tunnel lost its socket".to_owned()))?;
                self.handshake(tcp, target, guard, deadline)
            }
            (Scheme::Https, None) => self.handshake(tcp, target, guard, deadline),
        }
    }

    fn handshake(
        &self,
        tcp: std::net::TcpStream,
        target: &Uri,
        guard: &Arc<dyn Guard>,
        deadline: Deadline,
    ) -> Result<Stream, Error> {
        let config = self
            .tls_config
            .get(self.trust.as_ref())
            .map_err(Error::Invalid)?;
        Ok(Stream::start_tls(
            tcp,
            config,
            target.host(),
            Arc::clone(guard),
            deadline,
        )?)
    }
}

/// Issue `CONNECT host:port`, with the proxy's Basic credential when it has
/// one, and require a 2xx before handing the socket to TLS.
fn connect_tunnel(
    stream: &mut Stream,
    target: &Uri,
    basic_auth: Option<&str>,
) -> Result<(), Error> {
    let authority = target.authority();
    let mut request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: keep-alive\r\n"
    );
    if let Some(credential) = basic_auth {
        request.push_str(&format!("Proxy-Authorization: Basic {credential}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    // Unbuffered on purpose: a BufReader would read past the blank line and
    // drop the first bytes of the TLS ServerHello. A 2xx's framing headers
    // are ignored (RFC 9110 §9.3.6).
    let head = read_head(stream)?;
    if !(200..300).contains(&head.code) {
        return Err(Error::Protocol(format!(
            "proxy refused CONNECT with status {}",
            head.code
        )));
    }
    Ok(())
}

/// Render the request head. The target is origin-form normally, absolute-form
/// when speaking plain HTTP to a proxy (RFC 9112 §3.2.2), and only then does
/// the head carry the proxy's credential: inside a `CONNECT` tunnel the origin
/// reads every header.
fn render_request(
    method: &str,
    target: &Uri,
    via: Option<&ProxyTarget>,
    headers: &[(String, String)],
    body_len: usize,
) -> String {
    let absolute_via = via.filter(|_| target.scheme() == Scheme::Http);
    let request_target = if absolute_via.is_some() {
        format!(
            "{}://{}{}",
            target.scheme().as_str(),
            target.host_header(),
            target.path_and_query()
        )
    } else {
        target.path_and_query().to_owned()
    };
    let mut head = format!("{method} {request_target} HTTP/1.1\r\n");
    head.push_str(&format!("Host: {}\r\n", target.host_header()));
    if let Some(credential) = absolute_via.and_then(ProxyTarget::basic_auth) {
        head.push_str(&format!("Proxy-Authorization: Basic {credential}\r\n"));
    }
    // One request per connection: no pool to keep warm, and an explicit close
    // makes the response's end unambiguous.
    head.push_str("Connection: close\r\n");
    // A bodiless GET or HEAD carries no length (RFC 9110 §8.6: a user agent
    // SHOULD NOT send one when the method anticipates no content); every
    // other request, an empty POST included, names its length.
    if body_len > 0 || !matches!(method, "GET" | "HEAD") {
        head.push_str(&format!("Content-Length: {body_len}\r\n"));
    }
    head.push_str("Accept: */*\r\n");
    for (name, value) in headers {
        // A caller header never duplicates a generated one: two `Host` or
        // `Content-Length` lines is a request-smuggling signature, and a second
        // `Connection` could revive keep-alive under close framing.
        if RESERVED_HEADERS
            .iter()
            .any(|generated| name.eq_ignore_ascii_case(generated))
        {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    head
}

/// Headers whose wire meaning [`render_request`] owns. The body always uses
/// Content-Length, and the proxy credential is the proxy URL's alone.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-authorization",
];

/// HTTP field names are nonempty tokens, shared by request validation and
/// response parsing so neither accepts separators the other rejects.
fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn valid_header_value(value: &str) -> bool {
    value.bytes().all(|b| b == b'\t' || b >= 0x20 && b != 0x7f)
}

/// Reject a header whose name or value could terminate the header block early.
fn validate_header(name: &str, value: &str) -> Result<(), Error> {
    if !valid_header_name(name) {
        return Err(Error::Invalid(format!("invalid header name: {name:?}")));
    }
    if !valid_header_value(value) {
        return Err(Error::Invalid(format!(
            "header {name} has a value containing a control character"
        )));
    }
    Ok(())
}

/// A parsed response.
#[derive(Debug)]
pub struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    /// The HTTP status code.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Whether the status is 2xx.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// The first value of `name`, matched case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The response body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// A final response head: its status and its header fields.
struct Head {
    code: u16,
    headers: Vec<(String, String)>,
}

/// Read one CRLF-terminated line, enforcing the header-block budget.
fn read_line<R: Read>(reader: &mut R, consumed: &mut usize) -> Result<String, Error> {
    let mut raw = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let n = reader.read(&mut byte)?;
        if n == 0 {
            if raw.is_empty() {
                return Err(Error::Protocol("connection closed mid-header".to_owned()));
            }
            break;
        }
        *consumed += 1;
        if *consumed > MAX_HEADER_BYTES {
            return Err(Error::Protocol("header block too large".to_owned()));
        }
        if byte[0] == b'\n' {
            break;
        }
        if raw.len() >= MAX_HEADER_LINE {
            return Err(Error::Protocol("header line too long".to_owned()));
        }
        raw.push(byte[0]);
    }
    if raw.last() == Some(&b'\r') {
        raw.pop();
    }
    String::from_utf8(raw).map_err(|_| Error::Protocol("header line is not UTF-8".to_owned()))
}

/// A number in HTTP's grammar — `1*DIGIT` (radix 10) or `1*HEXDIG` (radix 16).
/// Rust's integer parsers also take a leading `+`, so every number read off
/// the wire goes through here. Overflow is `None`.
fn parse_http_number(text: &str, radix: u32) -> Option<usize> {
    if text.is_empty() || !text.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    usize::from_str_radix(text, radix).ok()
}

/// The status code of one status line. RFC 9112 §4 makes it exactly `3DIGIT`
/// and RFC 9110 §15 puts it in 100..=599; `u16::from_str` alone would also take
/// `+200`, `0200` and `65535`.
fn read_status_line<R: Read>(reader: &mut R, consumed: &mut usize) -> Result<u16, Error> {
    let line = read_line(reader, consumed)?;
    let mut parts = line.splitn(3, ' ');
    let version = parts
        .next()
        .ok_or_else(|| Error::Protocol("empty status line".to_owned()))?;
    if !version.starts_with("HTTP/1.") {
        return Err(Error::Protocol(format!("unsupported version {version:?}")));
    }
    parts
        .next()
        .filter(|code| code.len() == 3)
        .and_then(|code| parse_http_number(code, 10))
        .and_then(|code| u16::try_from(code).ok())
        .filter(|code| (100..=599).contains(code))
        .ok_or_else(|| Error::Protocol(format!("unparseable status line {line:?}")))
}

/// The header fields up to the empty line that ends a head.
fn read_header_fields<R: Read>(
    reader: &mut R,
    consumed: &mut usize,
) -> Result<Vec<(String, String)>, Error> {
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader, consumed)?;
        if line.is_empty() {
            return Ok(headers);
        }
        if headers.len() >= MAX_HEADER_COUNT {
            return Err(Error::Protocol("too many response headers".to_owned()));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| Error::Protocol(format!("header without a colon: {line:?}")))?;
        if !valid_header_name(name) || !valid_header_value(value) {
            return Err(Error::Protocol("invalid response header field".to_owned()));
        }
        headers.push((name.to_owned(), value.trim_matches([' ', '\t']).to_owned()));
    }
}

/// Read the final response head, skipping every interim (1xx) one before it.
///
/// RFC 9110 §15.2: a client must parse 1xx responses it did not ask for
/// (`100 Continue`, `103 Early Hints`); taking the first head would return a
/// 1xx with the real response as its body and make a CONNECT look refused.
/// `101` is an error: this client never asks to upgrade. All heads share one
/// [`MAX_HEADER_BYTES`] budget, so a peer cannot stream 1xx heads forever.
///
/// A peer that closes before its first byte sent no response: [`Error::Io`]
/// (`UnexpectedEof`), never [`Error::Protocol`], so a caller tells "nothing
/// answered" (a later try may get past it) from "what answered is not HTTP".
fn read_head<R: Read>(reader: &mut R) -> Result<Head, Error> {
    let mut consumed = 0usize;
    loop {
        let code = match read_status_line(reader, &mut consumed) {
            // Every byte read counts against the budget, so none was read.
            Err(Error::Protocol(_)) if consumed == 0 => {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the connection closed before any response",
                )));
            }
            code => code?,
        };
        let headers = read_header_fields(reader, &mut consumed)?;
        match code {
            101 => {
                return Err(Error::Protocol(
                    "unrequested 101 Switching Protocols".to_owned(),
                ));
            }
            100..=199 => {}
            _ => return Ok(Head { code, headers }),
        }
    }
}

/// Read status line, headers and body from `stream`.
fn read_response(stream: Stream, limit: usize) -> Result<Response, Error> {
    read_response_from(&mut BufReader::new(stream), limit)
}

/// The response parser over any buffered reader, so the unit tests run the
/// same framing path as the live socket.
fn read_response_from<R: BufRead>(reader: &mut R, limit: usize) -> Result<Response, Error> {
    let Head { code, headers } = read_head(reader)?;
    let body = match decide_framing(code, &headers)? {
        Framing::Empty => Vec::new(),
        Framing::Chunked => read_chunked(reader, limit)?,
        Framing::Length(declared) => {
            if declared > limit {
                return Err(Error::TooLarge { limit });
            }
            let mut body = vec![0u8; declared];
            reader.read_exact(&mut body)?;
            body
        }
        Framing::ToEof => read_to_limit(reader, limit)?,
    };
    Ok(Response {
        status: code,
        headers,
        body,
    })
}

/// How the response body is delimited. One function decides this, because a
/// body whose end two parties disagree about is the whole smuggling class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Framing {
    /// No content: the response ends at its header block (1xx, 204, 304).
    Empty,
    /// `Transfer-Encoding: chunked`.
    Chunked,
    /// `Content-Length`, already parsed and agreed across duplicates.
    Length(usize),
    /// Neither header: the body runs to EOF (we always send `Connection: close`).
    ToEof,
}

/// Resolve the response framing (RFC 9112 §6.3), refusing every ambiguous
/// combination.
///
/// 1xx, 204 and 304 end at their header block whatever they declare (rule 1).
/// Refused: `Transfer-Encoding` with `Content-Length` (rule 3), disagreeing
/// duplicate `Content-Length`s, a length that is not `1*DIGIT`, and any coding
/// list other than exactly `chunked` — this client decodes nothing else, and a
/// non-final chunked does not frame the body (rule 4). Every
/// `Transfer-Encoding` header counts: `gzip` then `chunked` is `gzip, chunked`.
fn decide_framing(status: u16, headers: &[(String, String)]) -> Result<Framing, Error> {
    /// Every value under `name`, not just the first: duplicates are the point.
    fn values<'a>(headers: &'a [(String, String)], name: &'a str) -> impl Iterator<Item = &'a str> {
        headers
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    if matches!(status, 100..=199 | 204 | 304) {
        return Ok(Framing::Empty);
    }
    let transfer_encoded = values(headers, "transfer-encoding").next().is_some();
    // The codings in order across every header, empty list elements dropped
    // (RFC 9110 §5.6.1). Exactly one, `chunked`, is the one list decoded here.
    let mut codings = values(headers, "transfer-encoding")
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let only_chunked = codings
        .next()
        .is_some_and(|t| t.eq_ignore_ascii_case("chunked"))
        && codings.next().is_none();

    let mut declared: Option<usize> = None;
    for text in values(headers, "content-length") {
        let parsed = parse_http_number(text, 10).ok_or_else(|| {
            let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
            Error::Protocol(if digits {
                format!("Content-Length {text:?} is too large")
            } else {
                format!("Content-Length is not a decimal byte count: {text:?}")
            })
        })?;
        // Repeating the same value is harmless; a different one leaves two parties
        // disagreeing about where this response ends.
        match declared {
            Some(first) if first != parsed => {
                return Err(Error::Protocol(
                    "response carries conflicting Content-Length headers".to_owned(),
                ));
            }
            _ => declared = Some(parsed),
        }
    }

    if !transfer_encoded {
        return Ok(declared.map_or(Framing::ToEof, Framing::Length));
    }
    // Both framings at once is the request-smuggling signature. Refuse.
    if declared.is_some() {
        return Err(Error::Protocol(
            "response carries both Content-Length and Transfer-Encoding".to_owned(),
        ));
    }
    if !only_chunked {
        return Err(Error::Protocol(
            "response uses a transfer coding other than chunked".to_owned(),
        ));
    }
    Ok(Framing::Chunked)
}

/// Read to EOF, erroring rather than truncating past `limit`.
fn read_to_limit<R: Read>(reader: &mut R, limit: usize) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            return Ok(body);
        }
        if body.len() + n > limit {
            return Err(Error::TooLarge { limit });
        }
        body.extend_from_slice(&chunk[..n]);
    }
}

/// Decode `Transfer-Encoding: chunked`.
fn read_chunked<R: Read>(reader: &mut R, limit: usize) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    let mut header_budget = 0usize;
    loop {
        let line = read_line(reader, &mut header_budget)?;
        // A chunk-size line may carry `;ext` parameters, and BWS before the
        // first `;` (RFC 9112 §7.1.1) — but nothing before the size itself.
        let size_text = line
            .split(';')
            .next()
            .unwrap_or("")
            .trim_end_matches([' ', '\t']);
        let size = parse_http_number(size_text, 16)
            .ok_or_else(|| Error::Protocol(format!("unparseable chunk size {size_text:?}")))?;
        if size == 0 {
            // Trailer section, then done.
            let mut trailer_budget = 0usize;
            loop {
                if read_line(reader, &mut trailer_budget)?.is_empty() {
                    break;
                }
            }
            return Ok(body);
        }
        // CHECKED, because `size` came off the wire: it is parsed with
        // `usize::from_str_radix(.., 16)` from the chunk-size line, so a peer can
        // name up to usize::MAX. Unchecked, `body.len() + size` panics outright in
        // debug and WRAPS in release — and a wrapped sum slips under the `> limit`
        // cap, makes `resize` TRUNCATE rather than grow, and then `read_exact`
        // panics on the slice range. Either way the calling worker thread unwinds
        // and stays dead for the life of the process, taking whatever it owns with
        // it. Reachable from any endpoint a caller is pointed at, or from
        // whatever won the race for a loopback port.
        let total = body
            .len()
            .checked_add(size)
            .ok_or(Error::TooLarge { limit })?;
        if total > limit {
            return Err(Error::TooLarge { limit });
        }
        let start = body.len();
        body.resize(total, 0);
        reader.read_exact(&mut body[start..])?;
        // Each chunk is followed by its own CRLF.
        let mut crlf = 0usize;
        let line = read_line(reader, &mut crlf)?;
        if !line.is_empty() {
            return Err(Error::Protocol("chunk not terminated by CRLF".to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wait for something that must happen is a hang detector, never a latency budget.
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    fn uri(text: &str) -> Uri {
        Uri::parse(text).unwrap()
    }

    fn proxy(text: &str) -> ProxyTarget {
        proxy::parse_proxy("HTTPS_PROXY", text).unwrap()
    }

    /// TCP reads may stop anywhere in the head or body. Drain the declared
    /// request before replying/closing, or an unread tail can reset the socket
    /// while the client is still writing (including macOS EINVAL on timeouts).
    fn read_plain_request(reader: &mut impl Read) -> Vec<u8> {
        const LIMIT: usize = 1024;
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            assert!(
                head.len() < LIMIT,
                "fixture request head exceeded its bound"
            );
            let mut byte = [0];
            reader.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = std::str::from_utf8(&head).unwrap();
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .expect("fixture request carries Content-Length");
        assert!(length <= LIMIT, "fixture request body exceeded its bound");
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        body
    }

    fn plain_stub(response: &'static [u8]) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/request", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
            stream.set_write_timeout(Some(HANG_DETECTOR)).unwrap();
            let body = read_plain_request(&mut stream);
            stream.write_all(response).unwrap();
            body
        });
        (endpoint, server)
    }

    #[test]
    fn repeated_https_requests_and_client_clones_build_one_tls_config() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("https://{}/request", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            // Closing immediately makes each TLS handshake fail after the client
            // has obtained its configuration; no private test key is required.
            for _ in 0..2 {
                drop(listener.accept().unwrap().0);
            }
        });
        let client = Client::new(
            Trust::Roots(vec![include_bytes!("testdata/tls/root.der").to_vec()]),
            ProxyMode::Direct,
            Duration::from_secs(2),
        );
        let clone = client.clone();

        assert!(
            Arc::ptr_eq(&client.trust, &clone.trust),
            "Client::clone must not copy configured CA bytes"
        );

        for requester in [&client, &clone] {
            requester
                .post(&endpoint)
                .send(b"{}")
                .expect_err("the TLS peer deliberately closes during handshake");
        }
        server.join().unwrap();

        assert!(Arc::ptr_eq(&client.tls_config, &clone.tls_config));
        assert_eq!(client.tls_config.builds(), 1);
    }

    #[test]
    fn shared_trust_constructor_retains_the_callers_root_allocation() {
        let trust = Arc::new(Trust::Roots(vec![vec![1, 2, 3, 4]]));
        let client = Client::with_shared_trust(
            Arc::clone(&trust),
            ProxyMode::Direct,
            Duration::from_secs(2),
        );
        let clone = client.clone();

        assert!(Arc::ptr_eq(&trust, &client.trust));
        assert!(Arc::ptr_eq(&client.trust, &clone.trust));
    }

    #[test]
    fn cached_tls_configuration_errors_are_shared_by_clones() {
        let client = Client::new(
            Trust::Roots(Vec::new()),
            ProxyMode::Direct,
            Duration::from_secs(2),
        );
        let clone = client.clone();
        let first = client.tls_config.get(client.trust.as_ref()).unwrap_err();
        let second = clone.tls_config.get(clone.trust.as_ref()).unwrap_err();
        assert_eq!(first, second);
        assert_eq!(client.tls_config.builds(), 1);
    }

    #[test]
    fn plain_http_never_constructs_the_deferred_tls_configuration() {
        let (endpoint, server) =
            plain_stub(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        // Unusable trust: only an HTTPS request may build the lazy TLS config.
        let client = Client::new(Trust::Roots(Vec::new()), ProxyMode::Direct, HANG_DETECTOR);
        let response = client.post(&endpoint).send(b"{}").unwrap();
        assert_eq!(server.join().unwrap(), b"{}");
        assert_eq!(response.body(), b"ok");
        assert_eq!(client.tls_config.builds(), 0);
    }

    #[test]
    fn a_plain_request_is_origin_form_with_host_and_length() {
        let head = render_request(
            "POST",
            &uri("http://127.0.0.1:11434/api/chat"),
            None,
            &[],
            7,
        );
        assert!(head.starts_with("POST /api/chat HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Host: 127.0.0.1:11434\r\n"), "{head}");
        assert!(head.contains("Content-Length: 7\r\n"), "{head}");
        assert!(head.contains("Connection: close\r\n"), "{head}");
        assert!(head.ends_with("\r\n\r\n"), "{head}");
    }

    #[test]
    fn a_bodiless_get_names_no_length_and_every_other_request_does() {
        let target = uri("http://127.0.0.1:7484/v1/alab?after=3");
        let get = render_request("GET", &target, None, &[], 0);
        assert!(
            get.starts_with("GET /v1/alab?after=3 HTTP/1.1\r\n"),
            "{get}"
        );
        assert!(
            !get.to_ascii_lowercase().contains("content-length"),
            "{get}"
        );
        assert!(get.contains("Connection: close\r\n"), "{get}");
        assert!(get.ends_with("\r\n\r\n"), "{get}");
        // A caller's own length cannot sneak one back in.
        let caller = [("Content-Length".to_owned(), "5".to_owned())];
        let get = render_request("GET", &target, None, &caller, 0);
        assert!(
            !get.to_ascii_lowercase().contains("content-length"),
            "{get}"
        );
        // An empty PUT or POST still frames its (empty) body.
        for method in ["PUT", "POST"] {
            let head = render_request(method, &target, None, &[], 0);
            assert!(head.starts_with(&format!("{method} /v1/alab?after=3 ")));
            assert!(head.contains("Content-Length: 0\r\n"), "{head}");
        }
        // A GET handed a body frames it rather than leaving it unframed.
        let get = render_request("GET", &target, None, &[], 2);
        assert!(get.contains("Content-Length: 2\r\n"), "{get}");
    }

    #[test]
    fn plain_http_through_a_proxy_uses_absolute_form() {
        let head = render_request(
            "POST",
            &uri("http://api.test/v1/x"),
            Some(&proxy("http://proxy.test:3128")),
            &[],
            0,
        );
        assert!(
            head.starts_with("POST http://api.test/v1/x HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(!head.contains("Proxy-Authorization"), "{head}");
    }

    #[test]
    fn the_proxy_credential_rides_the_absolute_form_and_never_the_tunnel() {
        let via = proxy("http://alice:s3cret@proxy.test:3128");
        // A caller's own Proxy-Authorization is dropped in both shapes: it
        // would duplicate the proxy URL's, or reach the origin.
        let caller = [(
            "proxy-authorization".to_owned(),
            "Basic Y2FsbGVy".to_owned(),
        )];
        let head = render_request("POST", &uri("http://api.test/v1/x"), Some(&via), &caller, 0);
        // base64("alice:s3cret"), exactly once.
        assert_eq!(
            head.matches("Proxy-Authorization: Basic YWxpY2U6czNjcmV0\r\n")
                .count(),
            1,
            "{head}"
        );
        assert_eq!(
            head.to_ascii_lowercase()
                .matches("proxy-authorization:")
                .count(),
            1,
            "{head}"
        );
        // Inside a CONNECT tunnel the origin reads the head: no credential.
        let head = render_request(
            "POST",
            &uri("https://api.test/v1/x"),
            Some(&via),
            &caller,
            0,
        );
        assert!(
            !head.to_ascii_lowercase().contains("proxy-authorization"),
            "{head}"
        );
    }

    #[test]
    fn https_keeps_origin_form_even_through_a_proxy() {
        // The proxy sees only CONNECT; the request itself goes inside the
        // tunnel and must not leak the absolute URL.
        let head = render_request(
            "POST",
            &uri("https://api.test/v1/x"),
            Some(&proxy("http://proxy.test:3128")),
            &[],
            0,
        );
        assert!(head.starts_with("POST /v1/x HTTP/1.1\r\n"), "{head}");
    }

    #[test]
    fn a_caller_cannot_duplicate_a_generated_header() {
        let head = render_request(
            "POST",
            &uri("http://a.test/x"),
            None,
            &[
                ("Host".to_owned(), "evil.test".to_owned()),
                ("Content-Length".to_owned(), "0".to_owned()),
                ("tRaNsFeR-EnCoDiNg".to_owned(), "chunked".to_owned()),
                ("connection".to_owned(), "keep-alive".to_owned()),
                ("Content-Type".to_owned(), "application/json".to_owned()),
            ],
            9,
        );
        assert_eq!(head.matches("Host:").count(), 1, "{head}");
        assert!(head.contains("Host: a.test\r\n"), "{head}");
        assert_eq!(head.matches("Content-Length:").count(), 1, "{head}");
        assert!(head.contains("Content-Length: 9\r\n"), "{head}");
        assert!(
            !head.to_ascii_lowercase().contains("transfer-encoding:"),
            "{head}"
        );
        assert_eq!(
            head.to_lowercase().matches("connection:").count(),
            1,
            "{head}"
        );
        assert!(head.contains("Connection: close\r\n"), "{head}");
        // A header that does not collide passes through.
        assert!(
            head.contains("Content-Type: application/json\r\n"),
            "{head}"
        );
    }

    #[test]
    fn header_injection_is_rejected() {
        for (name, value) in [
            ("X-A", "ok\r\nX-Evil: 1"),
            ("X-A", "ok\nX-Evil: 1"),
            ("X-A", "ok\0"),
            ("X\r\nEvil", "1"),
            ("", "1"),
            ("X:Y", "1"),
        ] {
            assert!(
                validate_header(name, value).is_err(),
                "{name:?}: {value:?} must be rejected"
            );
        }
        // A bearer token and a content type are ordinary values.
        assert!(validate_header("Authorization", "Bearer abc.def-123").is_ok());
        assert!(validate_header("Content-Type", "application/json").is_ok());
    }

    #[test]
    fn header_names_use_the_same_token_grammar_in_both_directions() {
        const PUNCTUATION: &str = "!#$%&'*+-.^_`|~";
        for byte in 0u8..=127 {
            let name = format!("X{}Field", char::from(byte));
            let valid = byte.is_ascii_alphanumeric() || PUNCTUATION.contains(char::from(byte));
            assert_eq!(validate_header(&name, "ok").is_ok(), valid, "{name:?}");
            // On the wire a colon ends the name, so X:Field: ok is the
            // legitimate field X with the value Field: ok.
            let valid_response = valid || byte == b':';
            let raw = format!("HTTP/1.1 204 No Content\r\n{name}: ok\r\n\r\n");
            assert_eq!(
                parse(raw.as_bytes(), 1024).is_ok(),
                valid_response,
                "{name:?}"
            );
        }
        for name in ["", "Content-Length ", " Content-Length", "X-é"] {
            assert!(validate_header(name, "0").is_err(), "{name:?}");
            let raw = format!("HTTP/1.1 204 No Content\r\n{name}: 0\r\n\r\n");
            assert!(
                matches!(parse(raw.as_bytes(), 1024), Err(Error::Protocol(_))),
                "{name:?}"
            );
        }
    }

    #[test]
    fn response_headers_preserve_values_and_reject_controls() {
        for byte in (0u8..=31).chain([127]).filter(|b| *b != b'\t') {
            let mut raw = b"HTTP/1.1 204 No Content\r\nX-Field: before".to_vec();
            raw.push(byte);
            raw.extend_from_slice(b"after\r\n\r\n");
            assert!(
                matches!(parse(&raw, 1024), Err(Error::Protocol(_))),
                "{byte}"
            );
            assert!(
                validate_header("X-Field", &format!("before{}after", char::from(byte))).is_err()
            );
        }
        let raw = "HTTP/1.1 204 No Content\r\nX-Field: \t\u{a0}value\u{a0}\t \r\n\r\n";
        let response = parse(raw.as_bytes(), 1024).unwrap();
        assert_eq!(response.header("x-field"), Some("\u{a0}value\u{a0}"));
    }

    fn parse(raw: &[u8], limit: usize) -> Result<Response, Error> {
        read_response_from(&mut BufReader::new(raw), limit)
    }

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn framing_refuses_every_response_whose_end_is_ambiguous() {
        assert_eq!(decide_framing(200, &headers(&[])).unwrap(), Framing::ToEof);
        assert_eq!(
            decide_framing(200, &headers(&[("Content-Length", "7")])).unwrap(),
            Framing::Length(7)
        );
        assert_eq!(
            decide_framing(200, &headers(&[("Transfer-Encoding", "chunked")])).unwrap(),
            Framing::Chunked
        );
        // Repeating the same length is legal; disagreeing is not.
        assert_eq!(
            decide_framing(
                200,
                &headers(&[("Content-Length", "5"), ("content-length", "5")])
            )
            .unwrap(),
            Framing::Length(5)
        );
        // A bodiless status ignores whatever framing it declares.
        assert_eq!(
            decide_framing(204, &headers(&[("Content-Length", "5")])).unwrap(),
            Framing::Empty
        );
        for ambiguous in [
            vec![("Content-Length", "5"), ("Content-Length", "9")],
            vec![("Content-Length", "5"), ("Transfer-Encoding", "chunked")],
            vec![("Content-Length", "5"), ("Transfer-Encoding", "gzip")],
            // Two Transfer-Encoding headers are the list `gzip, chunked`.
            vec![
                ("Transfer-Encoding", "gzip"),
                ("Transfer-Encoding", "chunked"),
            ],
            vec![
                ("Transfer-Encoding", "gzip"),
                ("Transfer-Encoding", "chunked"),
                ("Content-Length", "5"),
            ],
            // RFC 9112 allows 1*DIGIT only; Rust's usize parser is laxer.
            vec![("Content-Length", "+5")],
            vec![("Content-Length", "5 ")],
            vec![("Content-Length", "")],
        ] {
            assert!(
                matches!(
                    decide_framing(200, &headers(&ambiguous)),
                    Err(Error::Protocol(_))
                ),
                "{ambiguous:?} must be refused"
            );
        }
        // The refusal names what is wrong with the value, overflow apart.
        let message =
            |value: &str| match decide_framing(200, &headers(&[("Content-Length", value)])) {
                Err(Error::Protocol(m)) => m,
                other => panic!("{value:?}: {other:?}"),
            };
        assert_eq!(
            message("+5"),
            "Content-Length is not a decimal byte count: \"+5\""
        );
        let huge = "9".repeat(40);
        assert_eq!(
            message(&huge),
            format!("Content-Length {huge:?} is too large")
        );
    }

    #[test]
    fn the_parser_frames_through_decide_framing() {
        // A response decide_framing refuses must be refused end to end too.
        for raw in [
            &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 9\r\n\r\nhi"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\nhi"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n"[..],
        ] {
            assert!(
                matches!(parse(raw, 1024), Err(Error::Protocol(_))),
                "{:?} must be refused",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn content_length_framing() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"a\":\"hello\"}";
        let response = parse(raw, 1024).unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.is_success());
        assert_eq!(response.header("content-type").unwrap(), "application/json");
        assert_eq!(response.body(), b"{\"a\":\"hello\"}");
    }

    #[test]
    fn chunked_framing_reassembles_and_skips_extensions_and_trailers() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
5;name=v\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: t\r\n\r\n";
        let response = parse(raw, 1024).unwrap();
        assert_eq!(response.body(), b"hello world");
    }

    #[test]
    fn a_chunk_size_is_hex_digits_and_nothing_else() {
        // RFC 9112 §7.1: chunk-size = 1*HEXDIG, with BWS allowed only before a
        // chunk extension's `;`.
        for size in ["+2", " 2", "\t2", "0x2", ""] {
            let raw = format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{size}\r\nhi\r\n0\r\n\r\n"
            );
            assert!(
                matches!(parse(raw.as_bytes(), 1024), Err(Error::Protocol(_))),
                "chunk size {size:?} must be refused"
            );
        }
        let spaced_extension =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2 ;name=v\r\nhi\r\n0\r\n\r\n";
        assert_eq!(parse(spaced_extension, 1024).unwrap().body(), b"hi");
    }

    #[test]
    fn eof_framing_when_no_length_is_given() {
        let raw = b"HTTP/1.1 200 OK\r\n\r\nbare body";
        assert_eq!(parse(raw, 1024).unwrap().body(), b"bare body");
    }

    #[test]
    fn both_framings_together_is_refused_as_smuggling() {
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
        assert!(matches!(parse(raw, 1024), Err(Error::Protocol(_))));
    }

    #[test]
    fn a_body_over_the_limit_errors_rather_than_truncating() {
        // Truncating would hand the caller a prefix that might still parse as
        // JSON — worse than failing.
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n";
        assert!(matches!(parse(raw, 10), Err(Error::TooLarge { limit: 10 })));

        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n20\r\n00000000000000000000000000000000\r\n0\r\n\r\n";
        assert!(matches!(parse(chunked, 10), Err(Error::TooLarge { .. })));

        let eof = b"HTTP/1.1 200 OK\r\n\r\n0123456789abcdef";
        assert!(matches!(parse(eof, 10), Err(Error::TooLarge { .. })));
    }

    #[test]
    fn non_2xx_status_is_reported_not_hidden() {
        let raw = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n";
        let response = parse(raw, 1024).unwrap();
        assert_eq!(response.status(), 503);
        assert!(!response.is_success());
    }

    #[test]
    fn malformed_status_lines_are_rejected() {
        for raw in [
            &b"garbage\r\n\r\n"[..],
            &b"HTTP/2 200 OK\r\n\r\n"[..],
            &b"HTTP/1.1 notanumber OK\r\n\r\n"[..],
        ] {
            assert!(parse(raw, 1024).is_err(), "{raw:?} must not parse");
        }
    }

    #[test]
    fn interim_1xx_responses_are_skipped_to_the_final_one() {
        // RFC 9110 §15.2: 1xx heads may precede the final response, even unasked.
        let raw = b"HTTP/1.1 100 Continue\r\n\r\n\
HTTP/1.1 103 Early Hints\r\nLink: </style.css>; rel=preload\r\n\r\n\
HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let response = parse(raw, 1024).unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"ok");
        // An interim head's fields are not the final response's.
        assert_eq!(response.header("link"), None);
        // 101 is never interim here: this client never asks to upgrade.
        let upgrade = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: h2c\r\n\r\n";
        assert!(matches!(parse(upgrade, 1024), Err(Error::Protocol(_))));
        // Interim heads share the head budget: an endless stream of them ends.
        let endless = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(MAX_HEADER_BYTES);
        assert!(matches!(parse(&endless, 1024), Err(Error::Protocol(_))));
    }

    #[test]
    fn a_peer_that_closes_unanswered_is_a_transport_failure() {
        // No byte at all is no response: the transport's failure, as a reset is.
        assert!(
            matches!(parse(b"", 1024), Err(Error::Io(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof),
            "{:?}",
            parse(b"", 1024)
        );
        // A head cut short once it began did answer, and is malformed; so is
        // the end after an interim head.
        for raw in [
            &b"HTTP/1.1 200 OK\r\n"[..],
            b"HTTP/1.1 100 Continue\r\n\r\n",
            b"\r\n",
        ] {
            assert!(
                matches!(parse(raw, 1024), Err(Error::Protocol(_))),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn bodiless_statuses_end_at_their_header_block_whatever_they_declare() {
        // RFC 9112 §6.3 rule 1: 204 and 304 carry no content whatever their
        // framing headers declare; reading the length would wait for bytes that
        // never come.
        for raw in [
            &b"HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\n"[..],
            &b"HTTP/1.1 304 Not Modified\r\nContent-Length: 5\r\n\r\n"[..],
            &b"HTTP/1.1 304 Not Modified\r\nTransfer-Encoding: chunked\r\n\r\n"[..],
        ] {
            let response = parse(raw, 1024)
                .unwrap_or_else(|error| panic!("{:?}: {error:?}", String::from_utf8_lossy(raw)));
            assert!(response.body().is_empty());
        }
    }

    #[test]
    fn a_transfer_coding_this_client_cannot_decode_is_refused() {
        // Only a final `chunked` frames a body this client can decode.
        for raw in [
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nContent-Length: 2\r\n\r\nhi"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nhi"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, gzip\r\n\r\n2\r\nhi\r\n0\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n"[..],
        ] {
            assert!(
                matches!(parse(raw, 1024), Err(Error::Protocol(_))),
                "{:?} must be refused",
                String::from_utf8_lossy(raw)
            );
        }
        // An empty list element is not a coding (RFC 9110 §5.6.1).
        let listed = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: , chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n";
        assert_eq!(parse(listed, 1024).unwrap().body(), b"hi");
    }

    #[test]
    fn a_status_code_is_exactly_three_digits() {
        for raw in [
            &b"HTTP/1.1 +200 OK\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 0200 OK\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 20 OK\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 999 OK\r\nContent-Length: 0\r\n\r\n"[..],
        ] {
            assert!(
                matches!(parse(raw, 1024), Err(Error::Protocol(_))),
                "{:?} must be refused",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn an_unterminated_header_block_does_not_hang() {
        let raw = b"HTTP/1.1 200 OK\r\nX-A: 1\r\n";
        assert!(parse(raw, 1024).is_err());
    }
}
