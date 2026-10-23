// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Single-request HTTP/1.1 client over [`crate::uri`] and [`crate::proxy`], bounded by
//! [`MAX_HEADER_BYTES`] and [`RequestBuilder::limit`].

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use aterm_uds::secret::{Secret, scrub};

use crate::proxy::{self, EnvSource, ProcessEnv, ProxyMode, ProxyTarget};
use crate::stream::{AlwaysAuthorized, Connect, Deadline, Guard, Stream, TcpConnector};
use crate::tls::{self, Trust};
use crate::uri::{Scheme, Uri};

/// Cap on the response header block, headers and status line together.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;
/// Cap on a single header line.
const MAX_HEADER_LINE: usize = 8 * 1024;
/// Cap on the number of header lines.
const MAX_HEADER_COUNT: usize = 128;
/// The most a whole-body read reserves up front for a declared length.
const PREALLOC_CAP: usize = 1 << 20;

/// A failure anywhere in a request.
#[derive(Debug)]
pub enum Error {
    /// The endpoint, a header, or the configuration was not usable.
    Invalid(String),
    /// Transport failure.
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
    /// Whether this is the revoked-authority error, as opposed to a network fault.
    #[inline]
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

/// An HTTP request method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`: never carries a body or a `Content-Length`.
    Get,
    /// `HEAD`: as `GET`, and its answer has no body whatever it declares.
    Head,
    /// `POST`: always carries a `Content-Length`, 0 when empty.
    Post,
    /// `PATCH`: always carries a `Content-Length`, 0 when empty.
    Patch,
    /// `PUT`: always carries a `Content-Length`, 0 when empty.
    Put,
    /// `DELETE`: carries a `Content-Length` only with a non-empty body.
    Delete,
}

impl Method {
    /// The method's token on the request line.
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }

    /// Whether a request with this method may carry a body at all.
    #[inline]
    const fn takes_body(self) -> bool {
        !matches!(self, Self::Get | Self::Head)
    }

    /// Whether a request with this method sends `Content-Length` even when
    /// its body is empty.
    #[inline]
    const fn always_sends_length(self) -> bool {
        matches!(self, Self::Post | Self::Patch | Self::Put)
    }
}

/// A request body.
pub enum Body<'a> {
    /// No body.
    Empty,
    /// A body held whole in memory.
    Bytes(&'a [u8]),
    /// A streamed body of exactly `len` bytes (a DMG over 1 gb).
    Reader {
        /// The exact number of bytes the reader yields.
        len: u64,
        /// The bytes.
        reader: &'a mut dyn Read,
    },
}

impl Body<'_> {
    /// The body's length in bytes.
    fn len(&self) -> u64 {
        match self {
            Self::Empty => 0,
            Self::Bytes(bytes) => wide(bytes.len()),
            Self::Reader { len, .. } => *len,
        }
    }
}

impl std::fmt::Debug for Body<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Empty"),
            Self::Bytes(bytes) => write!(formatter, "Bytes({} bytes)", bytes.len()),
            Self::Reader { len, .. } => write!(formatter, "Reader({len} bytes)"),
        }
    }
}

/// How far a failed request got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// No byte of the request head was accepted by the stream.
    NotSent,
    /// The stream accepted request bytes.
    Sent,
    /// The status line and headers parsed.
    Answered,
}

/// A failed exchange: what failed, and how far the request got first.
#[derive(Debug)]
pub struct SendError {
    /// How far the request got.
    pub phase: Phase,
    /// What failed.
    pub error: Error,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.error)
    }
}

impl std::error::Error for SendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// A `usize` as a `u64`: lossless on every target this crate builds for, and
/// saturating rather than wrapping anywhere it would not be.
fn wide(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// A `u64` as a `usize`, saturating: a length to read at most.
fn narrow(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// One lazily-built TLS configuration, shared by every clone of a [`Client`].
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

    /// A client that shares an already-owned trust model.
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

    /// Replace the TCP connector.
    #[must_use]
    pub fn with_connector(mut self, connector: Arc<dyn Connect>) -> Self {
        self.connector = connector;
        self
    }

    /// Begin a POST to `endpoint`.
    #[must_use]
    pub fn post(&self, endpoint: &str) -> RequestBuilder<'_> {
        self.request(Method::Post, endpoint)
    }

    /// Begin a PUT to `endpoint`.
    #[must_use]
    pub fn put(&self, endpoint: &str) -> RequestBuilder<'_> {
        self.request(Method::Put, endpoint)
    }

    /// Begin a GET to `endpoint`.
    #[must_use]
    pub fn get(&self, endpoint: &str) -> RequestBuilder<'_> {
        self.request(Method::Get, endpoint)
    }

    /// Begin a `method` request to `endpoint`.
    #[must_use]
    pub fn request(&self, method: Method, endpoint: &str) -> RequestBuilder<'_> {
        RequestBuilder {
            client: self,
            method,
            endpoint: endpoint.to_owned(),
            headers: Vec::new(),
            secrets: Vec::new(),
            limit: usize::MAX,
            guard: None,
            idle: None,
            backstop: None,
            connect_within: None,
            error: None,
        }
    }
}

/// A request under construction.
pub struct RequestBuilder<'a> {
    client: &'a Client,
    method: Method,
    endpoint: String,
    headers: Vec<(String, String)>,
    /// Headers whose values are secrets.
    secrets: Vec<(String, Secret)>,
    limit: usize,
    guard: Option<Arc<dyn Guard>>,
    idle: Option<Duration>,
    backstop: Option<Duration>,
    connect_within: Option<Duration>,
    /// First construction error, surfaced at `send` so the builder stays chainable.
    error: Option<Error>,
}

/// Names, never values: a header set through [`RequestBuilder::header`] can
/// carry a credential too, and the endpoint can carry one in its query.
impl std::fmt::Debug for RequestBuilder<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        let secrets: Vec<&str> = self.secrets.iter().map(|(name, _)| name.as_str()).collect();
        formatter
            .debug_struct("RequestBuilder")
            .field("method", &self.method)
            .field("headers", &headers)
            .field("secret_headers", &secrets)
            .field("limit", &self.limit)
            .field("idle", &self.idle)
            .field("backstop", &self.backstop)
            .field("connect_within", &self.connect_within)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl RequestBuilder<'_> {
    /// Add a header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let Err(error) = validate_header(name, value) {
            self.error.get_or_insert(error);
            return self;
        }
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// A header whose value is a secret.
    #[must_use]
    pub fn secret_header(mut self, name: &str, value: Secret) -> Self {
        let refused = if !valid_header_name(name) {
            Some(Error::Invalid(format!("invalid header name: {name:?}")))
        } else if is_reserved(name) {
            Some(Error::Invalid(format!(
                "header {name} is generated by the client"
            )))
        } else if name.eq_ignore_ascii_case("accept") {
            Some(Error::Invalid(format!(
                "header {name} is never a secret: set it with header()"
            )))
        } else if !valid_header_bytes(value.expose()) {
            Some(Error::Invalid(format!(
                "header {name} has a value containing a control character"
            )))
        } else {
            None
        };
        match refused {
            Some(error) => {
                self.error.get_or_insert(error);
            }
            None => self.secrets.push((name.to_owned(), value)),
        }
        self
    }

    /// Bound the response body.
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

    /// No byte for `idle` while connecting, sending or reading fails the request `TimedOut`.
    #[must_use]
    pub const fn idle(mut self, idle: Duration) -> Self {
        self.idle = Some(idle);
        self
    }

    /// This request's total deadline, in place of the client's timeout.
    #[must_use]
    pub const fn backstop(mut self, total: Duration) -> Self {
        self.backstop = Some(total);
        self
    }

    /// The budget for DNS, connect, a proxy CONNECT and the TLS handshake, within the total.
    #[must_use]
    pub const fn connect_within(mut self, connect: Duration) -> Self {
        self.connect_within = Some(connect);
        self
    }

    /// Send `body` and read the whole response.
    ///
    /// # Errors
    /// Any configuration, transport, or protocol failure, or a body over the configured limit.
    pub fn send(self, body: &[u8]) -> Result<Response, Error> {
        self.send_with_env(body, &ProcessEnv)
    }

    /// [`Self::send`] against an injected environment source (tests).
    ///
    /// # Errors
    /// As [`Self::send`].
    pub fn send_with_env<E: EnvSource>(self, body: &[u8], env: &E) -> Result<Response, Error> {
        let Exchange {
            status,
            headers,
            body,
        } = self
            .exchange_with_env(Body::Bytes(body), env)
            .map_err(|failed| failed.error)?;
        Ok(Response {
            status,
            headers,
            body: read_to_end(body.inner)?,
        })
    }

    /// Send `body` and read the response head, leaving its body to be read from the [`Exchange`].
    ///
    /// # Errors
    /// A [`SendError`]: the failure, and how far the request got.
    pub fn exchange(self, body: Body<'_>) -> Result<Exchange, SendError> {
        self.exchange_with_env(body, &ProcessEnv)
    }

    /// [`Self::exchange`] against an injected environment source.
    ///
    /// # Errors
    /// As [`Self::exchange`].
    pub fn exchange_with_env<E: EnvSource>(
        self,
        body: Body<'_>,
        env: &E,
    ) -> Result<Exchange, SendError> {
        let not_sent = |error: Error| SendError {
            phase: Phase::NotSent,
            error,
        };
        let sent = |error: Error| SendError {
            phase: Phase::Sent,
            error,
        };
        let Self {
            client,
            method,
            endpoint,
            headers,
            secrets,
            limit,
            guard,
            idle,
            backstop,
            connect_within,
            error,
        } = self;
        if let Some(error) = error {
            return Err(not_sent(error));
        }
        let target = Uri::parse(&endpoint).ok_or_else(|| {
            not_sent(Error::Invalid(
                "endpoint is not a usable absolute http(s) URL".to_owned(),
            ))
        })?;
        let body_len = body.len();
        if body_len > 0 && !method.takes_body() {
            return Err(not_sent(Error::Invalid(format!(
                "a {} carries no body",
                method.as_str()
            ))));
        }
        let mut deadline = Deadline::after(backstop.unwrap_or(client.timeout));
        if let Some(idle) = idle {
            deadline = deadline.with_idle(idle);
        }
        let connect = connect_within.map_or(deadline, |within| deadline.connect_phase(within));
        let guard: Arc<dyn Guard> = guard.unwrap_or_else(|| Arc::new(AlwaysAuthorized));
        let via = proxy::resolve(client.proxy_mode, &target, env).map_err(not_sent)?;

        let mut stream = client
            .open(&target, via.as_ref(), &guard, connect)
            .map_err(not_sent)?;
        stream.set_deadline(deadline);
        let content_length = (method.always_sends_length() || body_len > 0).then_some(body_len);
        let mut head = render_head(
            method,
            &target,
            via.as_ref(),
            &headers,
            &secrets,
            content_length,
        );
        drop(secrets);
        let mut phase = Phase::NotSent;
        let wrote = write_tracked(&mut stream, &head, &mut phase);
        scrub(&mut head);
        drop(head);
        let written = match wrote {
            Err(error) if phase == Phase::NotSent => return Err(not_sent(Error::Io(error))),
            Err(error) => Err(Upload::Wire(error)),
            Ok(()) => {
                send_body(&mut stream, body).and_then(|()| stream.flush().map_err(Upload::Wire))
            }
        };
        let mut reader = BufReader::new(stream);
        let head = match written {
            Ok(()) => read_final_head(&mut reader).map_err(sent)?,
            Err(Upload::Body(error)) => return Err(sent(error)),
            Err(Upload::Wire(error)) => {
                early_answer(&mut reader, &error).ok_or_else(|| sent(Error::Io(error)))?
            }
        };
        let framing =
            decide_framing(method, head.code, &head.headers).map_err(|error| SendError {
                phase: Phase::Answered,
                error,
            })?;
        Ok(Exchange {
            status: head.code,
            headers: head.headers,
            body: BodyReader {
                inner: Framed::new(reader, framing, limit),
            },
        })
    }
}

/// Write all of `bytes`, moving `phase` to [`Phase::Sent`] at the first byte
/// the stream accepts.
fn write_tracked(stream: &mut Stream, mut bytes: &[u8], phase: &mut Phase) -> io::Result<()> {
    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "the connection accepted no more of the request",
                ));
            }
            Ok(written) => {
                *phase = Phase::Sent;
                bytes = bytes.get(written..).unwrap_or_default();
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Why the request's bytes stopped short of the peer.
enum Upload {
    /// The caller's body reader ended short or failed.
    Body(Error),
    /// The connection refused a write.
    Wire(io::Error),
}

/// Write the body after its head. A [`Body::Reader`] must yield exactly its
/// `len`: one that ends sooner is `UnexpectedEof`, and nothing past `len` is
/// asked of it.
fn send_body(stream: &mut Stream, body: Body<'_>) -> Result<(), Upload> {
    match body {
        Body::Empty => Ok(()),
        Body::Bytes(bytes) => stream.write_all(bytes).map_err(Upload::Wire),
        Body::Reader { len, reader } => {
            let mut buffer = vec![0u8; 64 * 1024];
            let mut sent = 0u64;
            while sent < len {
                let want = narrow(len - sent).min(buffer.len());
                let read = match reader.read(&mut buffer[..want]) {
                    Ok(0) => {
                        return Err(Upload::Body(Error::Io(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            format!("the body reader ended at {sent} of {len} bytes"),
                        ))));
                    }
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        return Err(Upload::Body(Error::Io(io::Error::other(format!(
                            "the body reader failed at {sent} of {len} bytes: {error}"
                        )))));
                    }
                };
                stream.write_all(&buffer[..read]).map_err(Upload::Wire)?;
                sent += wide(read);
            }
            Ok(())
        }
    }
}

/// The answer a server sent before it read the whole request, read after the
/// write it made fail (RFC 9112 §9.5: a client sending a body watches for an
/// error response).
fn early_answer<R: BufRead>(reader: &mut R, failed: &io::Error) -> Option<Head> {
    if matches!(
        failed.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::PermissionDenied
    ) {
        return None;
    }
    read_final_head(reader).ok()
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
        let (host, port) = match via {
            Some(p) => (p.uri().host().to_owned(), p.uri().port()),
            None => (target.host().to_owned(), target.port()),
        };
        let tcp = self.connector.connect(&host, port, deadline)?;
        match (target.scheme(), via) {
            (Scheme::Http, _) => Ok(Stream::plain(tcp, Arc::clone(guard), deadline)),
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
#[cfg(test)]
fn render_request(
    method: Method,
    target: &Uri,
    via: Option<&ProxyTarget>,
    headers: &[(String, String)],
    content_length: Option<u64>,
) -> String {
    let mut head = render_fields(method, target, via, headers, content_length);
    head.push_str("\r\n");
    head
}

/// The head [`render_fields`] renders, then each secret header after the
/// caller's headers, then the blank line, into a buffer sized up front so it
/// never reallocates: no copy of a secret is left behind in a freed
/// allocation.
fn render_head(
    method: Method,
    target: &Uri,
    via: Option<&ProxyTarget>,
    headers: &[(String, String)],
    secrets: &[(String, Secret)],
    content_length: Option<u64>,
) -> Vec<u8> {
    let fields = render_fields(method, target, via, headers, content_length);
    let size = fields.len()
        + secrets
            .iter()
            .map(|(name, value)| name.len() + 2 + value.len() + 2)
            .sum::<usize>()
        + 2;
    let mut head = Vec::with_capacity(size);
    head.extend_from_slice(fields.as_bytes());
    for (name, value) in secrets {
        head.extend_from_slice(name.as_bytes());
        head.extend_from_slice(b": ");
        head.extend_from_slice(value.expose());
        head.extend_from_slice(b"\r\n");
    }
    head.extend_from_slice(b"\r\n");
    head
}

/// Whether [`render_fields`] owns the header `name`, so a caller's is dropped.
fn is_reserved(name: &str) -> bool {
    RESERVED_HEADERS
        .iter()
        .any(|generated| name.eq_ignore_ascii_case(generated))
}

/// The request line and every header line, without the blank line that ends
/// the head.
fn render_fields(
    method: Method,
    target: &Uri,
    via: Option<&ProxyTarget>,
    headers: &[(String, String)],
    content_length: Option<u64>,
) -> String {
    let method = method.as_str();
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
    head.push_str("Connection: close\r\n");
    if let Some(length) = content_length {
        head.push_str(&format!("Content-Length: {length}\r\n"));
    }
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("accept"))
    {
        head.push_str("Accept: */*\r\n");
    }
    for (name, value) in headers {
        // A caller header never duplicates a generated one: two `Host` or
        // `Content-Length` lines is a request-smuggling signature, and a second
        // `Connection` could revive keep-alive under close framing.
        if is_reserved(name) {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head
}

/// Headers whose wire meaning [`render_fields`] owns. The body always uses
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
    valid_header_bytes(value.as_bytes())
}

/// A field value's bytes: HTAB, or anything from SP up except DEL.
fn valid_header_bytes(value: &[u8]) -> bool {
    value.iter().all(|&b| b == b'\t' || b >= 0x20 && b != 0x7f)
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
    #[inline]
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Whether the status is 2xx.
    #[inline]
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// The first value of `name`, matched case-insensitively.
    #[inline]
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header_values(&self.headers, name).next()
    }

    /// Every header as `(name, value)`, in the order the response sent them.
    #[inline]
    pub fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        header_pairs(&self.headers)
    }

    /// Every value of `name`, matched case-insensitively, in order.
    #[inline]
    pub fn header_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        header_values(&self.headers, name)
    }

    /// The response body.
    #[inline]
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// The pairs of a header list as string slices, in order.
fn header_pairs(headers: &[(String, String)]) -> impl Iterator<Item = (&str, &str)> {
    headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
}

/// EVERY value under `name` (ASCII case-insensitive), not just the first, in
/// order: duplicates are the point.
fn header_values<'h, 'n>(
    headers: &'h [(String, String)],
    name: &'n str,
) -> impl Iterator<Item = &'h str> + use<'h, 'n> {
    headers
        .iter()
        .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// A response whose head has been read and whose body is streamed.
pub struct Exchange {
    status: u16,
    headers: Vec<(String, String)>,
    body: BodyReader,
}

impl Exchange {
    /// The HTTP status code.
    #[inline]
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Every header as `(name, value)`, in the order the response sent them.
    #[inline]
    pub fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        header_pairs(&self.headers)
    }

    /// The first value of `name`, matched case-insensitively.
    #[inline]
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header_values(&self.headers, name).next()
    }

    /// Every value of `name`, matched case-insensitively, in order.
    #[inline]
    pub fn header_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        header_values(&self.headers, name)
    }

    /// The body, to be read incrementally.
    #[inline]
    #[must_use]
    pub fn into_body(self) -> BodyReader {
        self.body
    }
}

/// Header names, never values: a response header can carry a bearer
/// credential too (a signed `Location`, a token in a `Set-Cookie`).
impl std::fmt::Debug for Exchange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        formatter
            .debug_struct("Exchange")
            .field("status", &self.status)
            .field("headers", &names)
            .field("body", &self.body)
            .finish()
    }
}

/// A response body, read incrementally under the response's framing and the request's limit.
pub struct BodyReader {
    inner: Framed<BufReader<Stream>>,
}

impl Read for BodyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl BodyReader {
    /// How many body bytes have been read so far (after chunked decoding).
    #[must_use]
    pub const fn consumed(&self) -> u64 {
        self.inner.consumed
    }

    /// The length the response declared.
    #[must_use]
    pub fn declared_len(&self) -> Option<u64> {
        self.inner.declared_len()
    }
}

impl std::fmt::Debug for BodyReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BodyReader")
            .field("framing", &self.inner.framing)
            .field("limit", &self.inner.limit)
            .field("consumed", &self.inner.consumed)
            .finish_non_exhaustive()
    }
}

/// Where a chunked body's decoder stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chunk {
    /// At a chunk-size line.
    Size,
    /// Inside a chunk, with this many data bytes left.
    Data(u64),
    /// At the CRLF that ends a chunk's data.
    End,
    /// Past the last chunk and its trailer section.
    Done,
}

/// The framing decoder over any buffered reader: the live socket under a
/// [`BodyReader`], or the in-memory bytes the unit tests feed it, so both run
/// ONE framing path.
struct Framed<R> {
    reader: R,
    framing: Framing,
    limit: usize,
    consumed: u64,
    chunk: Chunk,
}

impl<R: BufRead> Framed<R> {
    const fn new(reader: R, framing: Framing, limit: usize) -> Self {
        Self {
            reader,
            framing,
            limit,
            consumed: 0,
            chunk: Chunk::Size,
        }
    }

    fn declared_len(&self) -> Option<u64> {
        match self.framing {
            Framing::Empty => Some(0),
            Framing::Length(declared) => Some(wide(declared)),
            Framing::Chunked | Framing::ToEof => None,
        }
    }

    /// The error of a body that would pass the limit.
    fn too_large(&self) -> io::Error {
        io::Error::other(Error::TooLarge { limit: self.limit })
    }

    /// Count `read` more body bytes, refusing a total past the limit.
    fn count(&mut self, read: usize) -> io::Result<usize> {
        let total = self.consumed.saturating_add(wide(read));
        if total > wide(self.limit) {
            return Err(self.too_large());
        }
        self.consumed = total;
        Ok(read)
    }

    fn read_chunked(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.chunk {
                Chunk::Done => return Ok(0),
                Chunk::Size => {
                    let line = chunk_line(&mut self.reader, &mut 0)?;
                    let size_text = line
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches([' ', '\t']);
                    let size = parse_http_number(size_text, 16).ok_or_else(|| {
                        bad_framing(format!("unparseable chunk size {size_text:?}"))
                    })?;
                    if size == 0 {
                        let mut budget = 0usize;
                        while !chunk_line(&mut self.reader, &mut budget)?.is_empty() {}
                        self.chunk = Chunk::Done;
                        return Ok(0);
                    }
                    let size = wide(size);
                    let fits = self
                        .consumed
                        .checked_add(size)
                        .is_some_and(|total| total <= wide(self.limit));
                    if !fits {
                        return Err(self.too_large());
                    }
                    self.chunk = Chunk::Data(size);
                }
                Chunk::Data(left) => {
                    let want = narrow(left).min(buf.len());
                    let read = self.reader.read(&mut buf[..want])?;
                    if read == 0 {
                        return Err(cut_short(format!(
                            "the response body ended inside a chunk, at {} bytes",
                            self.consumed
                        )));
                    }
                    let left = left - wide(read);
                    self.chunk = if left == 0 {
                        Chunk::End
                    } else {
                        Chunk::Data(left)
                    };
                    return self.count(read);
                }
                Chunk::End => {
                    if !chunk_line(&mut self.reader, &mut 0)?.is_empty() {
                        return Err(bad_framing("chunk not terminated by CRLF".to_owned()));
                    }
                    self.chunk = Chunk::Size;
                }
            }
        }
    }
}

impl<R: BufRead> Read for Framed<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self.framing {
            Framing::Empty => Ok(0),
            Framing::Length(declared) => {
                if declared > self.limit {
                    return Err(self.too_large());
                }
                let declared = wide(declared);
                let left = declared - self.consumed;
                if left == 0 {
                    return Ok(0);
                }
                let want = narrow(left).min(buf.len());
                let read = self.reader.read(&mut buf[..want])?;
                if read == 0 {
                    return Err(cut_short(format!(
                        "the response body ended at {} of {declared} bytes",
                        self.consumed
                    )));
                }
                self.count(read)
            }
            Framing::Chunked => self.read_chunked(buf),
            Framing::ToEof => {
                let read = self.reader.read(buf)?;
                self.count(read)
            }
        }
    }
}

/// A body cut short: `UnexpectedEof`.
fn cut_short(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, message)
}

/// Bad body framing: `InvalidData`, carrying the [`Error::Protocol`] that
/// [`RequestBuilder::send`] reports.
fn bad_framing(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, Error::Protocol(message))
}

/// One line of chunked framing (a chunk-size line, a chunk's closing CRLF, a
/// trailer field), at most [`MAX_HEADER_LINE`] bytes, with the bytes counted
/// against `budget` up to [`MAX_HEADER_BYTES`].
fn chunk_line<R: Read>(reader: &mut R, budget: &mut usize) -> io::Result<String> {
    let mut raw = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        if read_uninterrupted(reader, &mut byte)? == 0 {
            return Err(cut_short(
                "the response body ended inside its chunked framing".to_owned(),
            ));
        }
        *budget += 1;
        if *budget > MAX_HEADER_BYTES {
            return Err(bad_framing("chunk trailer section too large".to_owned()));
        }
        if byte[0] == b'\n' {
            break;
        }
        if raw.len() >= MAX_HEADER_LINE {
            return Err(bad_framing("chunk framing line too long".to_owned()));
        }
        raw.push(byte[0]);
    }
    if raw.last() == Some(&b'\r') {
        raw.pop();
    }
    String::from_utf8(raw).map_err(|_| bad_framing("chunk framing line is not UTF-8".to_owned()))
}

/// The [`Error`] a body read failed with: the one a [`Framed`] error carries
/// inside it (`TooLarge`, `Protocol`), else the I/O error itself.
fn body_error(error: io::Error) -> Error {
    let ours = error.get_ref().is_some_and(|inner| inner.is::<Error>());
    if !ours {
        return Error::Io(error);
    }
    let kind = error.kind();
    match error.into_inner().map(|inner| inner.downcast::<Error>()) {
        Some(Ok(inner)) => *inner,
        _ => Error::Io(io::Error::from(kind)),
    }
}

/// Read a whole body, as [`RequestBuilder::send`] returns it.
fn read_to_end<R: BufRead>(mut body: Framed<R>) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    if let Some(declared) = body.declared_len().filter(|n| *n <= wide(body.limit)) {
        let _ = bytes.try_reserve_exact(narrow(declared).min(PREALLOC_CAP));
    }
    body.read_to_end(&mut bytes).map_err(body_error)?;
    Ok(bytes)
}

/// A final response head: its status and its header fields.
#[derive(Debug)]
struct Head {
    code: u16,
    headers: Vec<(String, String)>,
}

/// Read the final response head off `reader`: everything between a written
/// request and the framing its headers declare.
fn read_final_head<R: BufRead>(reader: &mut R) -> Result<Head, Error> {
    let closed = loop {
        match reader.fill_buf() {
            Ok(buffered) => break buffered.is_empty(),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break true,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(Error::Io(error)),
        }
    };
    if closed {
        return Err(Error::Io(cut_short(
            "connection closed before the response".to_owned(),
        )));
    }
    read_head(reader)
}

/// One byte of a head, or `0` at its end. An `UnexpectedEof` read error is
/// that end too: it is how rustls reports a peer that closed without
/// `close_notify`, so a head cut short reads the same over TLS as over TCP.
fn read_head_byte<R: Read>(reader: &mut R, byte: &mut [u8; 1]) -> io::Result<usize> {
    match read_uninterrupted(reader, byte) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
        other => other,
    }
}

/// `reader.read(buf)`, retrying every `Interrupted` (the `Read` contract's
/// "this operation can be retried"), so no other error and no byte is lost.
fn read_uninterrupted<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(buf) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

/// Read one CRLF-terminated line, enforcing the header-block budget.
fn read_line<R: Read>(reader: &mut R, consumed: &mut usize) -> Result<String, Error> {
    let mut raw = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let n = read_head_byte(reader, &mut byte)?;
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
fn read_head<R: Read>(reader: &mut R) -> Result<Head, Error> {
    let mut consumed = 0usize;
    loop {
        let code = match read_status_line(reader, &mut consumed) {
            Err(Error::Protocol(_)) if consumed == 0 => {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the connection closed before any response",
                )));
            }
            code => code?,
        };
        let headers = read_header_fields(reader, &mut consumed)?;
        if code == 101 || !(100..=199).contains(&code) {
            return Ok(Head { code, headers });
        }
    }
}

/// The whole response parser over any buffered reader: the in-memory bytes
/// the unit tests feed it, through the same [`read_final_head`],
/// [`decide_framing`] and [`Framed`] a live exchange runs.
#[cfg(test)]
fn read_response_from<R: BufRead>(
    mut reader: R,
    method: Method,
    limit: usize,
) -> Result<Response, Error> {
    let Head { code, headers } = read_final_head(&mut reader)?;
    let framing = decide_framing(method, code, &headers)?;
    Ok(Response {
        status: code,
        headers,
        body: read_to_end(Framed::new(reader, framing, limit))?,
    })
}

/// How the response body is delimited. One function decides this, because a
/// body whose end two parties disagree about is the whole smuggling class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Framing {
    /// No content: the response ends at its header block (HEAD, 1xx, 204, 304).
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
fn decide_framing(
    method: Method,
    status: u16,
    headers: &[(String, String)],
) -> Result<Framing, Error> {
    // A final 101 switches to a protocol this client never asked for: no
    // framing of this one describes what follows.
    if status == 101 {
        return Err(Error::Protocol(
            "unrequested 101 Switching Protocols".to_owned(),
        ));
    }
    if method == Method::Head || matches!(status, 100..=199 | 204 | 304) {
        return Ok(Framing::Empty);
    }
    let transfer_encoded = header_values(headers, "transfer-encoding").next().is_some();
    let mut codings = header_values(headers, "transfer-encoding")
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let only_chunked = codings
        .next()
        .is_some_and(|t| t.eq_ignore_ascii_case("chunked"))
        && codings.next().is_none();

    let mut declared: Option<usize> = None;
    for text in header_values(headers, "content-length") {
        let parsed = parse_http_number(text, 10).ok_or_else(|| {
            let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
            Error::Protocol(if digits {
                format!("Content-Length {text:?} is too large")
            } else {
                format!("Content-Length is not a decimal byte count: {text:?}")
            })
        })?;
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
    /// while the client is still writing.
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
        let client = Client::new(Trust::Roots(Vec::new()), ProxyMode::Direct, HANG_DETECTOR);
        let response = client.post(&endpoint).send(b"{}").unwrap();
        assert_eq!(server.join().unwrap(), b"{}");
        assert_eq!(response.body(), b"ok");
        assert_eq!(client.tls_config.builds(), 0);
    }

    #[test]
    fn a_plain_request_is_origin_form_with_host_and_length() {
        let head = render_request(
            Method::Post,
            &uri("http://127.0.0.1:11434/api/chat"),
            None,
            &[],
            Some(7),
        );
        assert!(head.starts_with("POST /api/chat HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Host: 127.0.0.1:11434\r\n"), "{head}");
        assert!(head.contains("Content-Length: 7\r\n"), "{head}");
        assert!(head.contains("Connection: close\r\n"), "{head}");
        assert!(head.ends_with("\r\n\r\n"), "{head}");
    }

    #[test]
    fn plain_http_through_a_proxy_uses_absolute_form() {
        let head = render_request(
            Method::Post,
            &uri("http://api.test/v1/x"),
            Some(&proxy("http://proxy.test:3128")),
            &[],
            Some(0),
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
        let caller = [(
            "proxy-authorization".to_owned(),
            "Basic Y2FsbGVy".to_owned(),
        )];
        let head = render_request(
            Method::Post,
            &uri("http://api.test/v1/x"),
            Some(&via),
            &caller,
            Some(0),
        );
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
        let head = render_request(
            Method::Post,
            &uri("https://api.test/v1/x"),
            Some(&via),
            &caller,
            Some(0),
        );
        assert!(
            !head.to_ascii_lowercase().contains("proxy-authorization"),
            "{head}"
        );
    }

    #[test]
    fn https_keeps_origin_form_even_through_a_proxy() {
        let head = render_request(
            Method::Post,
            &uri("https://api.test/v1/x"),
            Some(&proxy("http://proxy.test:3128")),
            &[],
            Some(0),
        );
        assert!(head.starts_with("POST /v1/x HTTP/1.1\r\n"), "{head}");
    }

    #[test]
    fn a_caller_cannot_duplicate_a_generated_header() {
        let head = render_request(
            Method::Post,
            &uri("http://a.test/x"),
            None,
            &[
                ("Host".to_owned(), "evil.test".to_owned()),
                ("Content-Length".to_owned(), "0".to_owned()),
                ("tRaNsFeR-EnCoDiNg".to_owned(), "chunked".to_owned()),
                ("connection".to_owned(), "keep-alive".to_owned()),
                ("Content-Type".to_owned(), "application/json".to_owned()),
            ],
            Some(9),
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
        read_response_from(BufReader::new(raw), Method::Get, limit)
    }

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn framing_refuses_every_response_whose_end_is_ambiguous() {
        assert_eq!(
            decide_framing(Method::Get, 200, &headers(&[])).unwrap(),
            Framing::ToEof
        );
        assert_eq!(
            decide_framing(Method::Get, 200, &headers(&[("Content-Length", "7")])).unwrap(),
            Framing::Length(7)
        );
        assert_eq!(
            decide_framing(
                Method::Get,
                200,
                &headers(&[("Transfer-Encoding", "chunked")])
            )
            .unwrap(),
            Framing::Chunked
        );
        assert_eq!(
            decide_framing(
                Method::Get,
                200,
                &headers(&[("Content-Length", "5"), ("content-length", "5")])
            )
            .unwrap(),
            Framing::Length(5)
        );
        assert_eq!(
            decide_framing(Method::Get, 204, &headers(&[("Content-Length", "5")])).unwrap(),
            Framing::Empty
        );
        for ambiguous in [
            vec![("Content-Length", "5"), ("Content-Length", "9")],
            vec![("Content-Length", "5"), ("Transfer-Encoding", "chunked")],
            vec![("Content-Length", "5"), ("Transfer-Encoding", "gzip")],
            vec![
                ("Transfer-Encoding", "gzip"),
                ("Transfer-Encoding", "chunked"),
            ],
            vec![
                ("Transfer-Encoding", "gzip"),
                ("Transfer-Encoding", "chunked"),
                ("Content-Length", "5"),
            ],
            vec![("Content-Length", "+5")],
            vec![("Content-Length", "5 ")],
            vec![("Content-Length", "")],
        ] {
            assert!(
                matches!(
                    decide_framing(Method::Get, 200, &headers(&ambiguous)),
                    Err(Error::Protocol(_))
                ),
                "{ambiguous:?} must be refused"
            );
        }
        let message = |value: &str| match decide_framing(
            Method::Get,
            200,
            &headers(&[("Content-Length", value)]),
        ) {
            Err(Error::Protocol(m)) => m,
            other => panic!("{value:?}: {other:?}"),
        };
        let bad = message("+5");
        assert!(bad.contains("\"+5\""), "{bad}");
        let huge = "9".repeat(40);
        let too_large = message(&huge);
        assert!(too_large.contains(&huge), "{too_large}");
        assert_ne!(
            bad.replace("\"+5\"", ""),
            too_large.replace(&format!("{huge:?}"), "")
        );
    }

    #[test]
    fn the_parser_frames_through_decide_framing() {
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
        let raw = b"HTTP/1.1 100 Continue\r\n\r\n\
HTTP/1.1 103 Early Hints\r\nLink: </style.css>; rel=preload\r\n\r\n\
HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let response = parse(raw, 1024).unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"ok");
        assert_eq!(response.header("link"), None);
        let upgrade = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: h2c\r\n\r\n";
        assert!(matches!(parse(upgrade, 1024), Err(Error::Protocol(_))));
        let endless = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(MAX_HEADER_BYTES);
        assert!(matches!(parse(&endless, 1024), Err(Error::Protocol(_))));
    }

    #[test]
    fn a_peer_that_closes_unanswered_is_a_transport_failure() {
        assert!(
            matches!(parse(b"", 1024), Err(Error::Io(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof),
            "{:?}",
            parse(b"", 1024)
        );
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

    #[test]
    fn a_declared_length_near_usize_max_is_a_short_body_not_an_allocation() {
        for declared in [usize::MAX, usize::MAX / 2] {
            let raw = format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\n\r\nx");
            match parse(raw.as_bytes(), usize::MAX) {
                Err(Error::Io(error)) => {
                    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof, "{error}");
                }
                other => panic!("{declared}: {other:?}"),
            }
        }
    }

    /// A reader that yields `bytes`, then fails as rustls does when its peer
    /// closes TCP without `close_notify`.
    struct UncleanClose<'a>(&'a [u8]);

    impl Read for UncleanClose<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "peer closed connection without sending TLS close_notify",
                ));
            }
            let n = buf.len().min(self.0.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn an_unclean_tls_close_in_the_head_reads_as_the_plain_eof() {
        let over_tls = read_final_head(&mut BufReader::new(UncleanClose(b""))).unwrap_err();
        let over_tcp = read_final_head(&mut BufReader::new(&b""[..])).unwrap_err();
        let (Error::Io(over_tls), Error::Io(over_tcp)) = (&over_tls, &over_tcp) else {
            panic!("{over_tls:?} / {over_tcp:?}");
        };
        assert_eq!(over_tls.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(
            (over_tls.kind(), over_tls.to_string()),
            (over_tcp.kind(), over_tcp.to_string())
        );
        for (unclean, plain) in [
            (
                &b"HTTP/1.1 200 OK\r\nX-A:"[..],
                &b"HTTP/1.1 200 OK\r\nX-A:"[..],
            ),
            (&b"HTTP/1.1 200 OK\r\n"[..], &b"HTTP/1.1 200 OK\r\n"[..]),
        ] {
            let over_tls = read_final_head(&mut BufReader::new(UncleanClose(unclean)));
            let over_tcp = read_final_head(&mut BufReader::new(plain));
            assert!(
                matches!(
                    (&over_tls, &over_tcp),
                    (Err(Error::Protocol(a)), Err(Error::Protocol(b))) if a == b
                ),
                "{over_tls:?} / {over_tcp:?}"
            );
        }
    }

    /// A reader that hands out `parts` one per read and fails every other
    /// read `Interrupted`, as a socket read does when a signal lands (on
    /// Linux, any caught signal, once `SO_RCVTIMEO` is set).
    struct Interrupting(std::collections::VecDeque<&'static [u8]>, bool);

    impl Read for Interrupting {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.1) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let Some(part) = self.0.pop_front() else {
                return Ok(0);
            };
            buf[..part.len()].copy_from_slice(part);
            self.1 = true;
            Ok(part.len())
        }
    }

    #[test]
    fn an_interrupted_read_inside_a_chunk_size_line_loses_no_framing() {
        let inner = Interrupting(
            [&b"1"[..], b"0\r\n0123456789abcdef\r\n0\r\n\r\n"].into(),
            false,
        );
        let mut body = Vec::new();
        Framed::new(BufReader::new(inner), Framing::Chunked, usize::MAX)
            .read_to_end(&mut body)
            .unwrap();
        assert_eq!(body, b"0123456789abcdef");
    }

    #[test]
    fn an_interrupted_read_in_the_head_is_waited_out() {
        let inner = Interrupting(
            [&b"HTTP/1.1 2"[..], b"00 OK\r\nX-A: 1\r\n\r\n"].into(),
            true,
        );
        let head = read_final_head(&mut BufReader::new(inner)).unwrap();
        assert_eq!(head.code, 200);
        assert_eq!(head.headers, vec![("X-A".to_owned(), "1".to_owned())]);
    }
}
