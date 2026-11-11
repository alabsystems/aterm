// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Single-request HTTP/1.1 client over [`crate::uri`] and [`crate::proxy`].

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
const MAX_HEADER_LINE: usize = 8 * 1024;
const MAX_HEADER_COUNT: usize = 128;
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

    /// Whether a request with this method sends `Content-Length` even when its body is empty.
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

fn wide(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

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
            backstop: Backstop::Client,
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
    backstop: Backstop,
    connect_within: Option<Duration>,
    /// First construction error, surfaced at `send` so the builder stays chainable.
    error: Option<Error>,
}

#[derive(Clone, Copy, Debug)]
enum Backstop {
    Client,
    Within(Duration),
    None,
}

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
        self.backstop = Backstop::Within(total);
        self
    }

    /// Remove the whole-request wall deadline. Connect and idle bounds still apply when the
    /// caller sets them; this is for large transfers whose total bound is earned by progress.
    #[must_use]
    pub const fn without_backstop(mut self) -> Self {
        self.backstop = Backstop::None;
        self
    }

    /// The budget for DNS, connect, a proxy CONNECT and the TLS handshake, within the total.
    #[must_use]
    pub const fn connect_within(mut self, connect: Duration) -> Self {
        self.connect_within = Some(connect);
        self
    }

    /// Send `body` and read the whole response.
    /// # Errors
    /// Any configuration, transport, or protocol failure, or a body over the configured limit.
    pub fn send(self, body: &[u8]) -> Result<Response, Error> {
        self.send_with_env(body, &ProcessEnv)
    }

    /// [`Self::send`] against an injected environment source (tests).
    /// # Errors
    /// As [`Self::send`].
    pub fn send_with_env<E: EnvSource>(self, body: &[u8], env: &E) -> Result<Response, Error> {
        self.send_phased_with_env(Body::Bytes(body), env)
            .map_err(|failed| failed.error)
    }

    /// Send a known-length body and read the whole response under its configured limit.
    ///
    /// # Errors
    /// As [`Self::send`], including a streamed body that ends short or fails while reading.
    pub fn send_body(self, body: Body<'_>) -> Result<Response, Error> {
        self.send_phased_with_env(body, &ProcessEnv)
            .map_err(|failed| failed.error)
    }

    /// [`Self::send`], saying of a failure how far the request got: a long-poll whose deadline
    /// passed while the server held it failed [`Phase::Sent`].
    /// # Errors
    /// As [`Self::send`], with its [`Phase`].
    pub fn send_phased(self, body: &[u8]) -> Result<Response, SendError> {
        self.send_phased_with_env(Body::Bytes(body), &ProcessEnv)
    }

    fn send_phased_with_env<E: EnvSource>(
        self,
        body: Body<'_>,
        env: &E,
    ) -> Result<Response, SendError> {
        self.exchange_with_env(body, env)?.collect()
    }

    /// Send `body` and read the response head, leaving its body to be read from the [`Exchange`].
    /// # Errors
    /// A [`SendError`]: the failure, and how far the request got.
    pub fn exchange(self, body: Body<'_>) -> Result<Exchange, SendError> {
        self.exchange_with_env(body, &ProcessEnv)
    }

    /// [`Self::exchange`] against an injected environment source.
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
        let mut deadline = match backstop {
            Backstop::Client => Deadline::after(client.timeout),
            Backstop::Within(total) => Deadline::after(total),
            Backstop::None => Deadline::unbounded(),
        };
        if let Some(idle) = idle {
            deadline = deadline.with_idle(idle);
        }
        let guard: Arc<dyn Guard> = guard.unwrap_or_else(|| Arc::new(AlwaysAuthorized));
        let via = proxy::resolve(client.proxy_mode, &target, env).map_err(not_sent)?;
        // The connect budget starts once the proxy is chosen: reading its variables is no
        // part of the DNS, connect, CONNECT and handshake that `connect_within` bounds.
        let connect = connect_within.map_or(deadline, |within| deadline.connect_phase(within));

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

enum Upload {
    /// The caller's body reader ended short or failed.
    Body(Error),
    /// The connection refused a write.
    Wire(io::Error),
}

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

fn connect_tunnel(
    stream: &mut Stream,
    target: &Uri,
    basic_auth: Option<&str>,
) -> Result<(), Error> {
    let authority = target.authority();
    let fields = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: keep-alive\r\n"
    );
    let mut request = credentialed_head(&fields, basic_auth, &[]);
    let wrote = stream.write_all(&request).and_then(|()| stream.flush());
    scrub(&mut request);
    drop(request);
    wrote?;
    let head = read_head(stream)?;
    if !(200..300).contains(&head.code) {
        return Err(Error::Protocol(format!(
            "proxy refused CONNECT with status {}",
            head.code
        )));
    }
    Ok(())
}

/// Render the request head: [`render_head`] with no secret header.
#[cfg(test)]
fn render_request(
    method: Method,
    target: &Uri,
    via: Option<&ProxyTarget>,
    headers: &[(String, String)],
    content_length: Option<u64>,
) -> String {
    String::from_utf8(render_head(
        method,
        target,
        via,
        headers,
        &[],
        content_length,
    ))
    .expect("a head of text fields and a text credential is UTF-8")
}

fn render_head(
    method: Method,
    target: &Uri,
    via: Option<&ProxyTarget>,
    headers: &[(String, String)],
    secrets: &[(String, Secret)],
    content_length: Option<u64>,
) -> Vec<u8> {
    let fields = render_fields(method, target, via, headers, content_length);
    let credential = absolute_via(target, via).and_then(ProxyTarget::basic_auth);
    credentialed_head(&fields, credential, secrets)
}

/// The proxy a plain-http request names in absolute form: an https request tunnels instead, and
/// its credential rides only the CONNECT.
fn absolute_via<'a>(target: &Uri, via: Option<&'a ProxyTarget>) -> Option<&'a ProxyTarget> {
    via.filter(|_| target.scheme() == Scheme::Http)
}

/// `fields`, then the proxy credential and each secret header, then the empty line that ends the
/// head, in one buffer allocated at its final size, so no reallocation leaves a copy of a secret
/// byte behind; the caller scrubs it once it is written. `fields` holds no secret byte, so this
/// buffer is the one copy of each that rendering makes.
fn credentialed_head(
    fields: &str,
    credential: Option<&str>,
    secrets: &[(String, Secret)],
) -> Vec<u8> {
    const PROXY_AUTHORIZATION: &[u8] = b"Proxy-Authorization: Basic ";
    let size = fields.len()
        + credential.map_or(0, |credential| {
            PROXY_AUTHORIZATION.len() + credential.len() + 2
        })
        + secrets
            .iter()
            .map(|(name, value)| name.len() + 2 + value.len() + 2)
            .sum::<usize>()
        + 2;
    let mut head = Vec::with_capacity(size);
    head.extend_from_slice(fields.as_bytes());
    if let Some(credential) = credential {
        head.extend_from_slice(PROXY_AUTHORIZATION);
        head.extend_from_slice(credential.as_bytes());
        head.extend_from_slice(b"\r\n");
    }
    for (name, value) in secrets {
        head.extend_from_slice(name.as_bytes());
        head.extend_from_slice(b": ");
        head.extend_from_slice(value.expose());
        head.extend_from_slice(b"\r\n");
    }
    head.extend_from_slice(b"\r\n");
    head
}

fn is_reserved(name: &str) -> bool {
    RESERVED_HEADERS
        .iter()
        .any(|generated| name.eq_ignore_ascii_case(generated))
}

fn render_fields(
    method: Method,
    target: &Uri,
    via: Option<&ProxyTarget>,
    headers: &[(String, String)],
    content_length: Option<u64>,
) -> String {
    let method = method.as_str();
    let request_target = if absolute_via(target, via).is_some() {
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
        // A caller header never duplicates a generated one.
        if is_reserved(name) {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head
}

const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-authorization",
];

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

fn valid_header_bytes(value: &[u8]) -> bool {
    value.iter().all(|&b| b == b'\t' || b >= 0x20 && b != 0x7f)
}

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

    /// The response body, taken whole without a copy.
    #[inline]
    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

fn header_pairs(headers: &[(String, String)]) -> impl Iterator<Item = (&str, &str)> {
    headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
}

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

    /// Read this response's whole body under its existing framing, limit and deadline.
    ///
    /// # Errors
    /// Body framing or I/O failed, with [`Phase::Answered`].
    pub fn collect(self) -> Result<Response, SendError> {
        let Self {
            status,
            headers,
            body,
        } = self;
        let body = complete_body(body, |reader| read_to_end(&mut reader.inner))?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    /// Consume a body incrementally, then validate and drain anything left before returning.
    /// A consumer's successful early return cannot skip the response's remaining framing.
    ///
    /// # Errors
    /// The consumer or remaining body reports an I/O/framing failure, with [`Phase::Answered`].
    pub fn consume_body<T>(
        self,
        consume: impl FnOnce(&mut BodyReader) -> io::Result<T>,
    ) -> Result<T, SendError> {
        complete_body(self.body, |reader| consume(reader).map_err(body_error))
    }
}

fn complete_body<T>(
    mut body: BodyReader,
    consume: impl FnOnce(&mut BodyReader) -> Result<T, Error>,
) -> Result<T, SendError> {
    let answered = |error| SendError {
        phase: Phase::Answered,
        error,
    };
    let result = consume(&mut body).map_err(answered)?;
    let mut tail = [0; 16 * 1024];
    while !body.inner.is_finished() {
        match body.read(&mut tail) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(answered(body_error(error))),
        }
    }
    Ok(result)
}

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

struct Framed<R> {
    reader: R,
    framing: Framing,
    limit: usize,
    consumed: u64,
    chunk: Chunk,
    eof: bool,
}

impl<R: BufRead> Framed<R> {
    const fn new(reader: R, framing: Framing, limit: usize) -> Self {
        Self {
            reader,
            framing,
            limit,
            consumed: 0,
            chunk: Chunk::Size,
            eof: false,
        }
    }

    fn declared_len(&self) -> Option<u64> {
        match self.framing {
            Framing::Empty => Some(0),
            Framing::Length(declared) => Some(wide(declared)),
            Framing::Chunked | Framing::ToEof => None,
        }
    }

    fn is_finished(&self) -> bool {
        match self.framing {
            Framing::Empty => true,
            Framing::Length(declared) => declared <= self.limit && self.consumed == wide(declared),
            Framing::Chunked => matches!(self.chunk, Chunk::Done),
            Framing::ToEof => self.eof,
        }
    }

    fn too_large(&self) -> io::Error {
        io::Error::other(Error::TooLarge { limit: self.limit })
    }

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
                if self.eof {
                    return Ok(0);
                }
                let read = self.reader.read(buf)?;
                self.eof = read == 0;
                self.count(read)
            }
        }
    }
}

fn cut_short(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, message)
}

fn bad_framing(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, Error::Protocol(message))
}

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
    if raw.pop() != Some(b'\r') {
        return Err(bad_framing(
            "chunk framing line not terminated by CRLF".to_owned(),
        ));
    }
    if !valid_header_bytes(&raw) {
        return Err(bad_framing(
            "chunk framing line contains a control character".to_owned(),
        ));
    }
    String::from_utf8(raw).map_err(|_| bad_framing("chunk framing line is not UTF-8".to_owned()))
}

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

fn read_to_end<R: BufRead>(body: &mut Framed<R>) -> Result<Vec<u8>, Error> {
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

fn read_head_byte<R: Read>(reader: &mut R, byte: &mut [u8; 1]) -> io::Result<usize> {
    match read_uninterrupted(reader, byte) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
        other => other,
    }
}

fn read_uninterrupted<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(buf) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

fn read_line<R: Read>(reader: &mut R, consumed: &mut usize) -> Result<String, Error> {
    let mut raw = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let n = read_head_byte(reader, &mut byte)?;
        if n == 0 {
            // A line its '\n' never ended is no line: a head cut after its last "\r" would
            // otherwise read that "\r" as the empty line that ends the header block.
            return Err(Error::Protocol("connection closed mid-header".to_owned()));
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

fn parse_http_number(text: &str, radix: u32) -> Option<usize> {
    if text.is_empty() || !text.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    usize::from_str_radix(text, radix).ok()
}

fn read_status_line<R: Read>(reader: &mut R, consumed: &mut usize) -> Result<u16, Error> {
    let line = read_line(reader, consumed)?;
    let mut parts = line.splitn(3, ' ');
    let version = parts
        .next()
        .ok_or_else(|| Error::Protocol("empty status line".to_owned()))?;
    if !matches!(
        version.as_bytes(),
        [b'H', b'T', b'T', b'P', b'/', b'1', b'.', b'0'..=b'9']
    ) {
        return Err(Error::Protocol(format!("unsupported version {version:?}")));
    }
    let code = parts
        .next()
        .filter(|code| code.len() == 3)
        .and_then(|code| parse_http_number(code, 10))
        .and_then(|code| u16::try_from(code).ok())
        .filter(|code| (100..=599).contains(code))
        .ok_or_else(|| Error::Protocol(format!("unparseable status line {line:?}")))?;
    if !parts.next().is_none_or(valid_header_value) {
        return Err(Error::Protocol(
            "status line reason phrase contains a control character".to_owned(),
        ));
    }
    Ok(code)
}

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

/// The whole response parser over any buffered reader.
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
        body: read_to_end(&mut Framed::new(reader, framing, limit))?,
    })
}

/// How the response body is delimited.
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

fn decide_framing(
    method: Method,
    status: u16,
    headers: &[(String, String)],
) -> Result<Framing, Error> {
    // A final 101 switches to a protocol this client never asked for.
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
#[path = "client_tests.rs"]
mod tests;
