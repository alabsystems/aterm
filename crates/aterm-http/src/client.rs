// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HTTP/1.1 request building and response parsing.
//!
//! Scope is set by what the title-summary worker actually does: one request per
//! connection, a known-length body, `Connection: close`, and NO redirect
//! following (the retired client was configured `max_redirects(0)`, and a
//! summary endpoint that redirects is a misconfiguration, not something to
//! chase — chasing it is how a bearer token reaches a host the operator never
//! named). There is no connection pool and no HTTP/2, because one bounded
//! worker making one call at a time needs neither.
//!
//! # Hardening
//!
//! * Header names and values are REJECTED if they contain CR, LF or NUL, so a
//!   value built from configuration cannot inject a second request. The request
//!   target is likewise never re-encoded ([`crate::uri`]).
//! * The response body is bounded by [`RequestBuilder::limit`]; exceeding it is
//!   an error rather than a truncation, so a hostile or broken endpoint cannot
//!   drive this process into swap by streaming forever.
//! * The header block is bounded too: an endpoint that never stops sending
//!   headers hits [`MAX_HEADER_BYTES`] instead of growing a buffer without end.
//! * `Content-Length` and `Transfer-Encoding: chunked` arriving TOGETHER is a
//!   request-smuggling signature and is rejected outright.

use std::io::{BufReader, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use crate::proxy::{self, EnvSource, ProcessEnv, ProxyMode};
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

/// Client configuration shared by every request it issues.
pub struct Client {
    trust: Trust,
    proxy_mode: ProxyMode,
    timeout: Duration,
    connector: Arc<dyn Connect>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("trust", &self.trust)
            .field("proxy_mode", &self.proxy_mode)
            .field("timeout", &self.timeout)
            .field("connector", &self.connector)
            .finish()
    }
}

impl Client {
    /// A client with the given trust model, proxy policy and global timeout.
    #[must_use]
    pub fn new(trust: Trust, proxy_mode: ProxyMode, timeout: Duration) -> Self {
        Self {
            trust,
            proxy_mode,
            timeout,
            connector: Arc::new(TcpConnector),
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
        RequestBuilder {
            client: self,
            method: "POST",
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
            .ok_or_else(|| Error::Invalid(format!("unusable endpoint: {}", self.endpoint)))?;
        let deadline = Deadline::after(self.client.timeout);
        let guard: Arc<dyn Guard> = self
            .guard
            .unwrap_or_else(|| Arc::new(crate::stream::AlwaysAuthorized));
        let via = proxy::resolve(self.client.proxy_mode, &target, env);

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
        via: Option<&Uri>,
        guard: &Arc<dyn Guard>,
        deadline: Deadline,
    ) -> Result<Stream, Error> {
        let (host, port) = match via {
            Some(p) => (p.host().to_owned(), p.port()),
            None => (target.host().to_owned(), target.port()),
        };
        let tcp = self.connector.connect(&host, port, deadline)?;
        match (target.scheme(), via) {
            // Plain HTTP: the proxy (if any) is spoken to in absolute-form; no
            // tunnel is needed.
            (Scheme::Http, _) => Ok(Stream::plain(tcp, Arc::clone(guard), deadline)),
            // HTTPS through a proxy: CONNECT first, then handshake against the
            // ORIGIN name so the proxy cannot substitute its own certificate.
            (Scheme::Https, Some(_)) => {
                let mut plain = Stream::plain(tcp, Arc::clone(guard), deadline);
                connect_tunnel(&mut plain, target)?;
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
        let config = tls::client_config(&self.trust).map_err(Error::Invalid)?;
        Ok(Stream::start_tls(
            tcp,
            config,
            target.host(),
            Arc::clone(guard),
            deadline,
        )?)
    }
}

/// Issue `CONNECT host:port` and require a 2xx before handing the socket to TLS.
fn connect_tunnel(stream: &mut Stream, target: &Uri) -> Result<(), Error> {
    let authority = target.authority();
    let request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: keep-alive\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    // Read the tunnel response UNBUFFERED, one byte at a time. A BufReader here
    // would read past the blank line and swallow the first bytes of the peer's
    // TLS ServerHello into a buffer that is dropped with it — the handshake
    // would then stall on data that had already arrived.
    let status = read_status_line(stream)?;
    let mut consumed = 0usize;
    loop {
        let line = read_line(stream, &mut consumed)?;
        if line.is_empty() {
            break;
        }
    }
    if !(200..300).contains(&status.code) {
        return Err(Error::Protocol(format!(
            "proxy refused CONNECT with status {}",
            status.code
        )));
    }
    Ok(())
}

/// Render the request head. The target is origin-form normally, absolute-form
/// when speaking plain HTTP to a proxy (RFC 9112 §3.2.2).
fn render_request(
    method: &str,
    target: &Uri,
    via: Option<&Uri>,
    headers: &[(String, String)],
    body_len: usize,
) -> String {
    let request_target = if via.is_some() && target.scheme() == Scheme::Http {
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
    // One request per connection: no pool to keep warm, and an explicit close
    // makes the response's end unambiguous.
    head.push_str("Connection: close\r\n");
    head.push_str(&format!("Content-Length: {body_len}\r\n"));
    head.push_str("Accept: */*\r\n");
    for (name, value) in headers {
        // A caller-supplied header must never DUPLICATE one this function
        // generates. Two `Host` or two `Content-Length` lines is a
        // request-smuggling signature — intermediaries disagree about which
        // one is authoritative — and a second `Connection` header could
        // resurrect keep-alive under a body framing that assumes close.
        if GENERATED_HEADERS
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

/// Headers [`render_request`] emits itself; a caller cannot override or
/// duplicate one.
const GENERATED_HEADERS: &[&str] = &["host", "content-length", "connection"];

/// Reject a header whose name or value could terminate the header block early.
fn validate_header(name: &str, value: &str) -> Result<(), Error> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b':' | b'(' | b')' | b',' | b'/'))
    {
        return Err(Error::Invalid(format!("invalid header name: {name:?}")));
    }
    if value
        .bytes()
        .any(|b| b == b'\r' || b == b'\n' || b == 0 || (b < 0x20 && b != b'\t'))
    {
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

    /// Consume the response, yielding its body.
    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

struct StatusLine {
    code: u16,
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

fn read_status_line<R: Read>(reader: &mut R) -> Result<StatusLine, Error> {
    let mut consumed = 0usize;
    let line = read_line(reader, &mut consumed)?;
    let mut parts = line.splitn(3, ' ');
    let version = parts
        .next()
        .ok_or_else(|| Error::Protocol("empty status line".to_owned()))?;
    if !version.starts_with("HTTP/1.") {
        return Err(Error::Protocol(format!("unsupported version {version:?}")));
    }
    let code = parts
        .next()
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| Error::Protocol(format!("unparseable status line {line:?}")))?;
    Ok(StatusLine { code })
}

/// Read status line, headers and body from `stream`.
fn read_response(stream: Stream, limit: usize) -> Result<Response, Error> {
    let mut reader = BufReader::new(stream);
    let status = read_status_line(&mut reader)?;
    let mut consumed = 0usize;
    let mut headers = Vec::new();
    loop {
        let line = read_line(&mut reader, &mut consumed)?;
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADER_COUNT {
            return Err(Error::Protocol("too many response headers".to_owned()));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| Error::Protocol(format!("header without a colon: {line:?}")))?;
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }

    let find = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    let chunked = find("transfer-encoding").is_some_and(|v| {
        v.split(',')
            .any(|t| t.trim().eq_ignore_ascii_case("chunked"))
    });
    let content_length = find("content-length");
    // Both framings at once is the request-smuggling signature. Refuse.
    if chunked && content_length.is_some() {
        return Err(Error::Protocol(
            "response carries both Content-Length and chunked Transfer-Encoding".to_owned(),
        ));
    }

    let body = if chunked {
        read_chunked(&mut reader, limit)?
    } else if let Some(text) = content_length {
        let declared = text
            .parse::<usize>()
            .map_err(|_| Error::Protocol(format!("unparseable Content-Length {text:?}")))?;
        if declared > limit {
            return Err(Error::TooLarge { limit });
        }
        let mut body = vec![0u8; declared];
        reader.read_exact(&mut body)?;
        body
    } else {
        // No framing header: the body runs to EOF (we sent Connection: close).
        read_to_limit(&mut reader, limit)?
    };
    Ok(Response {
        status: status.code,
        headers,
        body,
    })
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
        // A chunk-size line may carry `;ext` parameters.
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| Error::Protocol(format!("unparseable chunk size {size_text:?}")))?;
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
        // panics on the slice range. Either way the title-summary worker thread
        // unwinds and stays dead for the life of the process, taking the managed
        // Ollama reap loop with it. Reachable from any endpoint the operator
        // configures, or from whatever won the race for a loopback port.
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

    fn uri(text: &str) -> Uri {
        Uri::parse(text).unwrap()
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
    fn plain_http_through_a_proxy_uses_absolute_form() {
        let head = render_request(
            "POST",
            &uri("http://api.test/v1/x"),
            Some(&uri("http://proxy.test:3128")),
            &[],
            0,
        );
        assert!(
            head.starts_with("POST http://api.test/v1/x HTTP/1.1\r\n"),
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
            Some(&uri("http://proxy.test:3128")),
            &[],
            0,
        );
        assert!(head.starts_with("POST /v1/x HTTP/1.1\r\n"), "{head}");
    }

    #[test]
    fn a_caller_cannot_duplicate_a_generated_header() {
        // Two Host or two Content-Length lines is a smuggling signature:
        // intermediaries disagree about which is authoritative.
        let head = render_request(
            "POST",
            &uri("http://a.test/x"),
            None,
            &[
                ("Host".to_owned(), "evil.test".to_owned()),
                ("Content-Length".to_owned(), "0".to_owned()),
                ("connection".to_owned(), "keep-alive".to_owned()),
                ("Content-Type".to_owned(), "application/json".to_owned()),
            ],
            9,
        );
        assert_eq!(head.matches("Host:").count(), 1, "{head}");
        assert!(head.contains("Host: a.test\r\n"), "{head}");
        assert_eq!(head.matches("Content-Length:").count(), 1, "{head}");
        assert!(head.contains("Content-Length: 9\r\n"), "{head}");
        assert_eq!(
            head.to_lowercase().matches("connection:").count(),
            1,
            "{head}"
        );
        assert!(head.contains("Connection: close\r\n"), "{head}");
        // A header that does NOT collide is still passed through.
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

    fn parse(raw: &[u8], limit: usize) -> Result<Response, Error> {
        // Drive the parser over a plain in-memory reader by reusing the same
        // functions read_response uses on a live socket.
        let mut reader = BufReader::new(raw);
        let status = read_status_line(&mut reader)?;
        let mut consumed = 0usize;
        let mut headers = Vec::new();
        loop {
            let line = read_line(&mut reader, &mut consumed)?;
            if line.is_empty() {
                break;
            }
            let (n, v) = line.split_once(':').unwrap();
            headers.push((n.trim().to_owned(), v.trim().to_owned()));
        }
        let find = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        };
        let chunked = find("transfer-encoding").is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("chunked"))
        });
        let content_length = find("content-length");
        if chunked && content_length.is_some() {
            return Err(Error::Protocol("both framings".to_owned()));
        }
        let body = if chunked {
            read_chunked(&mut reader, limit)?
        } else if let Some(text) = content_length {
            let declared = text.parse::<usize>().unwrap();
            if declared > limit {
                return Err(Error::TooLarge { limit });
            }
            let mut body = vec![0u8; declared];
            reader.read_exact(&mut body)?;
            body
        } else {
            read_to_limit(&mut reader, limit)?
        };
        Ok(Response {
            status: status.code,
            headers,
            body,
        })
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
    fn an_unterminated_header_block_does_not_hang() {
        let raw = b"HTTP/1.1 200 OK\r\nX-A: 1\r\n";
        assert!(parse(raw, 1024).is_err());
    }
}
